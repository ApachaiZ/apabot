//! Gestion systemd OPTIONNELLE (`apabot systemd install|remove`).
//!
//! Le superviseur embarqué est le mode par défaut (cross-platform, aucune
//! dépendance système). systemd reste disponible pour qui préfère la
//! supervision native Linux : cette commande génère et installe l'unité en
//! fonction des chemins RÉELS (binaire courant, répertoire courant, .env),
//! avec une DEMANDE DE VALIDATION à chaque étape irréversible — écrire un
//! fichier, recharger, activer. Rien n'est modifié sans confirmation, et
//! `--dry-run` affiche l'unité sans rien écrire.
//!
//! En cas d'installation systemd, ne PAS combiner avec `apabot start` :
//! deux superviseurs piloteraient deux bots sur la même gateway Discord.

use dialoguer::{Confirm, Select};
use std::path::PathBuf;

use crate::i18n::{fill, Catalog};

const UNIT_NAME: &str = "apabot";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    User,
    System,
}

impl Scope {
    fn flag(self) -> Option<&'static str> {
        match self {
            Scope::User => Some("--user"),
            Scope::System => None,
        }
    }

    fn wanted_by(self) -> &'static str {
        match self {
            Scope::User => "default.target",
            Scope::System => "multi-user.target",
        }
    }

    fn unit_path(self) -> Option<PathBuf> {
        match self {
            Scope::User => home_dir().map(|h| h.join(".config/systemd/user/apabot.service")),
            Scope::System => Some(PathBuf::from("/etc/systemd/system/apabot.service")),
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

/// Génère le contenu de l'unité pour les chemins réels de CETTE installation.
/// Pur et testable : aucun accès disque.
pub(crate) fn build_unit(exe: &str, cwd: &str, env_file: Option<&str>, scope: Scope) -> String {
    let env_line = env_file
        .map(|p| format!("EnvironmentFile=-{p}\n"))
        .unwrap_or_default();
    let cwd_abs = PathBuf::from(cwd)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(cwd));
    let read_write = format!(
        "ReadWritePaths=-{configd} -{logs}\n",
        configd = cwd_abs.join(".config.d").display(),
        logs = cwd_abs.join("logs").display(),
    );
    format!(
        "[Unit]\n\
         Description=Apach Game Control Discord Bot (Rust)\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe} run --non-interactive\n\
         WorkingDirectory={cwd}\n\
         {env_line}\
         Restart=on-failure\n\
         RestartSec=5s\n\
         StandardOutput=journal\n\
         StandardError=journal\n\
         SyslogIdentifier=apabot\n\
         \n\
         # Durcissement (identique au déploiement historique)\n\
         NoNewPrivileges=true\n\
         ProtectSystem=strict\n\
         ProtectHome=true\n\
         PrivateTmp=true\n\
         ProtectKernelTunables=true\n\
         ProtectKernelModules=true\n\
         ProtectControlGroups=true\n\
         RestrictSUIDSGID=true\n\
         LockPersonality=true\n\
         {read_write}\
         \n\
         [Install]\n\
         WantedBy={}\n",
        scope.wanted_by()
    )
}

fn systemctl(scope: Scope, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("systemctl");
    if let Some(flag) = scope.flag() {
        cmd.arg(flag);
    }
    let out = cmd.args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            format!("systemctl exited with {}", out.status)
        } else {
            err
        })
    }
}

/// Point d'entrée de `apabot systemd …`. `args[2]` = install|remove.
pub fn cmd_systemd(args: &[String], catalog: &Catalog) -> i32 {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        eprintln!("{}", catalog.ops.systemd_unsupported);
        return 1;
    }
    #[cfg(target_os = "linux")]
    {
        match args.get(2).map(String::as_str) {
            Some("install") => install(args, catalog),
            Some("remove") => remove(args, catalog),
            _ => {
                eprintln!("{}", catalog.ops.systemd_usage);
                2
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn install(args: &[String], catalog: &Catalog) -> i32 {
    let dry_run = args.iter().any(|a| a == "--dry-run");

    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let env_file = crate::paths::env_file().exists().then(|| crate::paths::env_file().display().to_string());

    // `--dry-run` : afficher l'unité SANS rien demander ni écrire (aucun
    // prompt — utilisable hors TTY, dans les scripts de CI par exemple).
    if dry_run {
        let unit = build_unit(&exe, &cwd, env_file.as_deref(), Scope::User);
        let path = Scope::User
            .unit_path()
            .unwrap_or_else(|| PathBuf::from("~/.config/systemd/user/apabot.service"));
        println!(
            "{}",
            fill(
                catalog.ops.systemd_preview,
                &[("path", &path.display().to_string()), ("unit", &unit)]
            )
        );
        println!("(system scope differs only by its install path and WantedBy=multi-user.target)");
        return 0;
    }

    let scopes = [Scope::User, Scope::System];
    let items = vec![catalog.ops.systemd_scope_user, catalog.ops.systemd_scope_system];
    let selection = Select::new()
        .with_prompt(catalog.ops.systemd_install_scope)
        .items(&items)
        .default(0)
        .interact();
    let Ok(selection) = selection else {
        println!("{}", catalog.ops.systemd_aborted);
        return 0;
    };
    let scope = scopes[selection];

    let Some(unit_path) = scope.unit_path() else {
        eprintln!("{}", catalog.ops.systemd_aborted);
        return 1;
    };
    if scope == Scope::System && !running_as_root() {
        eprintln!("{}", catalog.ops.systemd_root_needed);
        return 1;
    }

    let unit = build_unit(&exe, &cwd, env_file.as_deref(), scope);

    println!(
        "{}",
        fill(
            catalog.ops.systemd_preview,
            &[("path", &unit_path.display().to_string()), ("unit", &unit)]
        )
    );

    // ── 1. Écriture du fichier d'unité (irréversible → confirmation) ──
    let ok = Confirm::new()
        .with_prompt(catalog.ops.systemd_confirm_write)
        .default(true)
        .interact()
        .unwrap_or(false);
    if !ok {
        println!("{}", catalog.ops.systemd_aborted);
        return 0;
    }
    if let Some(parent) = unit_path.parent() {
        if std::fs::create_dir_all(parent).is_err() && !parent.exists() {
            eprintln!("{}", fill(catalog.ops.systemd_write_failed, &[("error", "mkdir")]));
            return 1;
        }
    }
    if let Err(e) = std::fs::write(&unit_path, &unit) {
        eprintln!("{}", fill(catalog.ops.systemd_write_failed, &[("error", &e.to_string())]));
        return 1;
    }
    println!("{}", fill(catalog.ops.systemd_written, &[("path", &unit_path.display().to_string())]));

    // ── 2. daemon-reload (confirmation) ──
    let ok = Confirm::new()
        .with_prompt(catalog.ops.systemd_confirm_reload)
        .default(true)
        .interact()
        .unwrap_or(false);
    if !ok {
        println!("{}", catalog.ops.systemd_aborted);
        return 0;
    }
    match systemctl(scope, &["daemon-reload"]) {
        Ok(_) => println!("{}", catalog.ops.systemd_reloaded),
        Err(e) => eprintln!("{}", fill(catalog.ops.systemd_reload_failed, &[("error", &e)])),
    }

    // ── 3. enable --now (confirmation) ──
    let ok = Confirm::new()
        .with_prompt(catalog.ops.systemd_confirm_enable)
        .default(true)
        .interact()
        .unwrap_or(false);
    if !ok {
        println!("{}", catalog.ops.systemd_aborted);
        return 0;
    }
    match systemctl(scope, &["enable", "--now", UNIT_NAME]) {
        Ok(_) => println!("{}", catalog.ops.systemd_enabled),
        Err(e) => {
            eprintln!("{}", fill(catalog.ops.systemd_enable_failed, &[("error", &e)]));
            return 1;
        }
    }
    0
}

#[cfg(target_os = "linux")]
fn remove(_args: &[String], catalog: &Catalog) -> i32 {
    // On cherche l'unité là où elle peut être installée.
    let candidates = [
        (Scope::System, Scope::System.unit_path()),
        (Scope::User, Scope::User.unit_path()),
    ];
    let Some((scope, path)) = candidates
        .into_iter()
        .find(|(_, p)| p.as_ref().map(|p| p.exists()).unwrap_or(false))
        .and_then(|(s, p)| p.map(|p| (s, p)))
    else {
        let fallback = Scope::System.unit_path().unwrap_or_default();
        println!(
            "{}",
            fill(catalog.ops.systemd_already_missing, &[("path", &fallback.display().to_string())])
        );
        return 0;
    };

    let ok = Confirm::new()
        .with_prompt(fill(catalog.ops.systemd_confirm_remove, &[("path", &path.display().to_string())]))
        .default(true)
        .interact()
        .unwrap_or(false);
    if !ok {
        println!("{}", catalog.ops.systemd_aborted);
        return 0;
    }
    let mut failed = false;
    match systemctl(scope, &["disable", "--now", UNIT_NAME]) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{}", fill(catalog.ops.systemd_remove_failed, &[("error", &e)]));
            failed = true;
        }
    }
    if let Err(e) = std::fs::remove_file(&path) {
        eprintln!("{}", fill(catalog.ops.systemd_remove_failed, &[("error", &e.to_string())]));
        failed = true;
    }
    if let Err(e) = systemctl(scope, &["daemon-reload"]) {
        eprintln!("{}", fill(catalog.ops.systemd_remove_failed, &[("error", &e)]));
        failed = true;
    }
    if failed {
        1
    } else {
        println!("{}", catalog.ops.systemd_removed);
        0
    }
}

#[cfg(target_os = "linux")]
fn running_as_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_contains_real_paths_and_hardening() {
        let unit = build_unit(
            "/opt/apabot/target/release/apabot",
            "/opt/apabot",
            Some("/opt/apabot/.config.d/.env"),
            Scope::System,
        );
        assert!(unit.contains("ExecStart=/opt/apabot/target/release/apabot run --non-interactive"));
        assert!(unit.contains("WorkingDirectory=/opt/apabot"));
        assert!(unit.contains("EnvironmentFile=-/opt/apabot/.config.d/.env"));
        assert!(unit.contains("NoNewPrivileges=true"));
        assert!(unit.contains("ProtectSystem=strict"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=multi-user.target"));
    }

    #[test]
    fn user_unit_targets_default_target_without_env() {
        let unit = build_unit("/exe", "/cwd", None, Scope::User);
        assert!(unit.contains("WantedBy=default.target"));
        assert!(!unit.contains("EnvironmentFile"));
    }
}
