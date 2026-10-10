//! Internationalisation (en / fr).
//!
//! Amélioration majeure par rapport à la version JS : les catalogues ne sont
//! PLUS des fichiers JSON interprétés au runtime, mais des **structs Rust
//! typées** (`src/i18n/en.rs`, `src/i18n/fr.rs`). Conséquences :
//!
//! 1. **Zéro coût runtime** : toutes les chaînes sont des `&'static str`
//!    gravées dans le binaire — pas de lecture fichier, pas de parse JSON,
//!    pas de recherche de clé par chemin (`"power.actions.start.verb"`).
//! 2. **Compilation = garde-fou** : une clé manquante ou renommée est une
//!    ERREUR DE COMPILATION, plus un bug découvert en production. La parité
//!    en/fr est vérifiée structurellement (les deux catalogues instancient
//!    le même type [`Catalog`]).
//! 3. Les textes avec variables (`{alias}`, `{seconds}`…) restent des gabarits
//!    interpolés par [`fill`] au moment de l'affichage.
//!
//! La langue est résolue au démarrage (`--lang=xx` > `BOT_LANGUAGE` > `en`)
//! et le catalogue devient une référence `&'static Catalog` **copiée** dans
//! l'état global — aucun verrou nécessaire pour lire les textes ensuite.

pub mod en;
pub mod fr;

/// Une « carte » d'embed : un titre + un corps (le motif le plus fréquent).
#[derive(Debug, Clone, Copy)]
pub struct Card {
    pub title: &'static str,
    pub body: &'static str,
}

/// Objet de catalogue réduit à un titre (ex. `common.generic_error`).
#[derive(Debug, Clone, Copy)]
pub struct TitleOnly {
    pub title: &'static str,
}

// ── Formes spécifiques du catalogue (parité stricte avec les locales JS) ──

#[derive(Debug, Clone, Copy)]
pub struct PowerAction {
    pub verb: &'static str,
    pub cap: &'static str,
    pub title: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Actions {
    pub start: PowerAction,
    pub stop: PowerAction,
    pub restart: PowerAction,
}

#[derive(Debug, Clone, Copy)]
pub struct PowerConfirm {
    pub warning: &'static str,
    pub body: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Completed {
    pub title: &'static str,
    pub body: &'static str,
    pub tail: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct NotConfirmed {
    pub title: &'static str,
    pub body_failed: &'static str,
    pub body_timeout: &'static str,
    pub body_transmit_error: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct PingFailed {
    pub title: &'static str,
    pub body: &'static str,
    pub status_suffix: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct ManageList {
    pub title_one: &'static str,
    pub title_many: &'static str,
    pub header: &'static str,
    pub entry: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct ManageClear {
    pub confirm_title: &'static str,
    pub confirm_body: &'static str,
    pub done_title: &'static str,
    pub done_body: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct NoChange {
    pub title: &'static str,
    pub body_add: &'static str,
    pub body_remove: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct ManageConfirm {
    pub title: &'static str,
    pub body: &'static str,
    pub verb_add: &'static str,
    pub verb_remove: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct ManageDone {
    pub title: &'static str,
    pub body_add: &'static str,
    pub body_remove: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct SetupPrompt {
    pub discord_token: &'static str,
    pub discord_client_id: &'static str,
    pub discord_owner_id: &'static str,
    pub discord_guild_id: &'static str,
    pub provider_api_key: &'static str,
    pub provider_service_id: &'static str,
    pub provider_services: &'static str,
}

// ── Sections du catalogue ──

#[derive(Debug, Clone, Copy)]
pub struct Common {
    pub access_denied_member: Card,
    pub access_denied_guild: Card,
    pub unknown_service: Card,
    pub session_reset: Card,
    /// Conservée pour la parité du catalogue JS : le framework poise aiguille
    /// les commandes par nom, une commande inconnue n'atteint jamais un
    /// handler Rust.
    #[allow(dead_code)]
    pub unhandled_command: Card,
    pub default_service: Card,
    pub generic_error: TitleOnly,
}

#[derive(Debug, Clone, Copy)]
pub struct Confirm {
    pub confirm: &'static str,
    pub cancel: &'static str,
    pub cancelled: Card,
    pub processing: Card,
    pub expired: Card,
}

#[derive(Debug, Clone, Copy)]
pub struct Power {
    pub actions: Actions,
    pub access_denied: Card,
    pub action_in_progress: Card,
    pub confirm: PowerConfirm,
    pub access_revoked: Card,
    pub busy: Card,
    pub already_in_target_state: Card,
    pub accepted: Card,
    pub accepted_after_error: Card,
    pub completed: Completed,
    pub applied_despite_error: Card,
    pub eta: &'static str,
    pub cancel_tracking: &'static str,
    pub tracking_cancelled: Card,
    pub not_confirmed: NotConfirmed,
}

#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub title: &'static str,
    pub cpu: &'static str,
    pub players: &'static str,
    pub uptime: &'static str,
    pub memory: &'static str,
    pub storage: &'static str,
    pub address: &'static str,
    pub node: &'static str,
    pub metrics: &'static str,
    pub not_reported: &'static str,
    pub state_running: &'static str,
    pub state_offline: &'static str,
    pub state_unknown: &'static str,
    /// Libellé du bouton « Rafraîchir » du /status (relecture sans retaper
    /// la commande).
    pub refresh: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Ping {
    pub failed: PingFailed,
    pub pong: Card,
}

#[derive(Debug, Clone, Copy)]
pub struct Manage {
    pub access_denied: Card,
    pub list: ManageList,
    pub clear: ManageClear,
    pub invalid: Card,
    /// Conservée pour la parité du catalogue JS : avec l'autocomplétion,
    /// l'option « membre » existe toujours (l'ancien défaut « commande en
    /// cache » du JS ne peut plus se produire).
    #[allow(dead_code)]
    pub stale_command: Card,
    pub no_change: NoChange,
    pub confirm: ManageConfirm,
    pub done: ManageDone,
}

#[derive(Debug, Clone, Copy)]
pub struct Logs {
    pub access_denied: Card,
    pub card_title: &'static str,
    pub not_found: &'static str,
    pub read_failed: &'static str,
    pub title_filtered: &'static str,
    pub title_last: &'static str,
    pub nothing_filtered: &'static str,
    pub empty: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Watchdog {
    pub down: Card,
    pub high_cpu: Card,
    pub high_ram: Card,
    pub high_disk: Card,
}

#[derive(Debug, Clone, Copy)]
pub struct Setup {
    pub intro: &'static str,
    pub file_line: &'static str,
    pub language_label: &'static str,
    pub prompt_line: &'static str,
    pub prompt_line_hidden: &'static str,
    pub prompt: SetupPrompt,
    pub invalid: &'static str,
    pub complete: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Errors {
    pub missing_config: &'static str,
    pub missing_service_id: &'static str,
    pub invalid_service: &'static str,
    pub request_failed: &'static str,
    pub request_failed_status: &'static str,
    pub request_failed_code: &'static str,
    pub request_failed_power_suffix: &'static str,
    pub request_failed_auth: &'static str,
    pub request_failed_forbidden: &'static str,
    pub request_failed_not_found: &'static str,
    pub request_failed_rate_limit: &'static str,
    pub request_failed_server: &'static str,
    pub request_failed_server_sent: &'static str,
    pub request_failed_timeout: &'static str,
    pub request_failed_timeout_sent: &'static str,
    pub request_failed_network: &'static str,
    pub request_failed_reset: &'static str,
    pub request_failed_reset_sent: &'static str,
    pub request_failed_generic: &'static str,
    pub setup_incomplete_non_interactive: &'static str,
    pub setup_incomplete_no_tty: &'static str,
    pub setup_incomplete_check: &'static str,
    pub invalid_users_json_shape: &'static str,
    pub deploy_missing_access: &'static str,
    pub deploy_unknown_application: &'static str,
    /// Carte montrée à l'utilisateur quand une commande PANIQUE (poise
    /// contient le panic — le bot survit, la carte explique).
    pub internal_panic: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Audit {
    pub power_requested: &'static str,
    pub power_send_failed: &'static str,
    pub power_send_ok: &'static str,
    pub power_tracking_cancelled: &'static str,
    pub power_applied_despite_error: &'static str,
    pub power_send_error_polling: &'static str,
    pub power_confirmed: &'static str,
    pub power_not_confirmed: &'static str,
    pub operator_granted: &'static str,
    pub operator_revoked: &'static str,
    pub operators_cleared: &'static str,
    pub commands_skipped: &'static str,
    pub commands_registered: &'static str,
    pub commands_global_purged: &'static str,
    pub commands_global_purge_failed: &'static str,
    pub commands_old_guild_purged: &'static str,
    pub commands_old_guild_purge_failed: &'static str,
    #[allow(dead_code)]
    pub commands_hash_failed: &'static str,
    pub watchdog_disabled: &'static str,
    pub watchdog_started: &'static str,
    pub watchdog_alert_sent: &'static str,
    pub watchdog_alert_failed: &'static str,
    pub watchdog_check_failed: &'static str,
    pub lock_write_failed: &'static str,
    pub sessions_write_failed: &'static str,
    pub users_invalid_entry: &'static str,
    pub users_not_array: &'static str,
}

/// Messages des outils d'exploitation (mission control) : commandes du
/// daemon, superviseur, systemd, TUI. Ce sont des textes de CONSOLE —
/// contrairement aux autres sections (embeds Discord), ils n'utilisent pas
/// de Markdown, juste des gabarits `{var}` et des `\n`.
#[derive(Debug, Clone, Copy)]
pub struct Ops {
    // ── Commandes CLI ──
    pub start_launching: &'static str,
    pub start_ok: &'static str,
    pub start_already: &'static str,
    pub start_stale: &'static str,
    pub not_responding: &'static str,
    pub stop_ok: &'static str,
    pub stop_failed: &'static str,
    pub not_running: &'static str,
    pub restart_ok: &'static str,
    pub restart_started: &'static str,
    pub status_stopped: &'static str,
    /// Titre de la carte « status » en console (cadre style bannière).
    pub status_card_title: &'static str,
    /// Libellé de l'état dans la carte status.
    pub status_state: &'static str,
    /// Titre de l'en-tête de la commande `logs`.
    pub logs_title: &'static str,
    /// Libellé du fichier dans l'en-tête `logs`.
    pub logs_file: &'static str,
    pub log_none: &'static str,
    pub usage: &'static str,
    // ── Superviseur (logs) ──
    pub sup_start: &'static str,
    pub sup_child_spawn_failed: &'static str,
    pub sup_child_exit: &'static str,
    pub sup_child_exit_stable: &'static str,
    pub sup_config_exit: &'static str,
    pub sup_max_restarts: &'static str,
    pub sup_stopping: &'static str,
    pub sup_force_kill: &'static str,
    pub sup_restart_requested: &'static str,
    pub sup_child_connected: &'static str,
    // ── systemd ──
    // (lu uniquement sous les OS sans systemd : jamais référencé sur Linux)
    #[allow(dead_code)]
    pub systemd_unsupported: &'static str,
    pub systemd_usage: &'static str,
    pub systemd_install_scope: &'static str,
    pub systemd_scope_user: &'static str,
    pub systemd_scope_system: &'static str,
    pub systemd_preview: &'static str,
    pub systemd_confirm_write: &'static str,
    pub systemd_written: &'static str,
    pub systemd_write_failed: &'static str,
    pub systemd_confirm_reload: &'static str,
    pub systemd_reloaded: &'static str,
    pub systemd_reload_failed: &'static str,
    pub systemd_confirm_enable: &'static str,
    pub systemd_enabled: &'static str,
    pub systemd_enable_failed: &'static str,
    pub systemd_confirm_remove: &'static str,
    pub systemd_removed: &'static str,
    pub systemd_remove_failed: &'static str,
    pub systemd_aborted: &'static str,
    pub systemd_already_missing: &'static str,
    pub systemd_root_needed: &'static str,
    // ── Installation utilisateur (install/reinstall/uninstall) ──
    pub install_usage: &'static str,
    pub completions_usage: &'static str,
    pub refresh_assets_ok: &'static str,
    pub install_unsupported: &'static str,
    pub install_scope_prompt: &'static str,
    pub install_scope_default: &'static str,
    pub install_scope_custom: &'static str,
    pub install_dest_prompt: &'static str,
    pub install_dest_invalid: &'static str,
    pub install_plan: &'static str,
    pub install_confirm: &'static str,
    pub install_ok: &'static str,
    pub install_already: &'static str,
    pub install_failed: &'static str,
    pub install_path_warn: &'static str,
    /// Nom d'instance généré (--name auto).
    pub install_name_generated: &'static str,
    /// Nom d'instance invalide.
    pub install_name_invalid: &'static str,
    pub reinstall_stop_prompt: &'static str,
    pub reinstall_confirm: &'static str,
    pub reinstall_ok: &'static str,
    pub uninstall_not_installed: &'static str,
    pub uninstall_confirm_bin: &'static str,
    pub uninstall_confirm_data: &'static str,
    pub uninstall_ok: &'static str,
    pub uninstall_ok_full: &'static str,
    pub uninstall_failed: &'static str,
    // ── Configuration interactive + profils ──
    pub config_usage: &'static str,
    pub config_updated: &'static str,
    pub config_no_env: &'static str,
    pub config_no_tty: &'static str,
    pub config_restart_prompt: &'static str,
    pub profile_saved: &'static str,
    pub profile_loaded: &'static str,
    pub profile_deleted: &'static str,
    pub profile_missing: &'static str,
    pub profile_list_header: &'static str,
    pub profile_none: &'static str,
    pub profile_confirm_load: &'static str,
    pub profile_confirm_delete: &'static str,
    pub profile_invalid_name: &'static str,
    pub install_configure_prompt: &'static str,
    pub install_env_todo: &'static str,
    pub install_env_file: &'static str,
    pub install_env_missing: &'static str,
    // ── TUI mission control ──
    pub tui_title: &'static str,
    pub tui_no_daemon: &'static str,
    pub tui_running: &'static str,
    pub tui_pid: &'static str,
    pub tui_uptime: &'static str,
    pub tui_restarts: &'static str,
    pub tui_bot: &'static str,
    pub tui_logs_title: &'static str,
    pub tui_follow: &'static str,
    pub tui_paused: &'static str,
    pub tui_prompt: &'static str,
    pub tui_keys: &'static str,
    pub tui_help: &'static str,
    pub tui_unknown_cmd: &'static str,
    pub tui_log_waiting: &'static str,
}

/// Le catalogue complet — c'est CE type que `en.rs` et `fr.rs` instancient :
/// toute divergence de forme entre les deux langues est une erreur de
/// compilation.
#[derive(Debug, Clone, Copy)]
pub struct Catalog {
    pub common: Common,
    pub confirm: Confirm,
    pub help: Card,
    pub power: Power,
    pub status: Status,
    pub ping: Ping,
    pub manage: Manage,
    pub logs: Logs,
    pub watchdog: Watchdog,
    pub setup: Setup,
    pub errors: Errors,
    pub audit: Audit,
    pub ops: Ops,
}

pub const SUPPORTED: [&str; 2] = ["en", "fr"];
pub const DEFAULT_LANG: &str = "en";

/// Résolution de la langue au démarrage, en cascade : `--lang=xx` (argument
/// CLI, prioritaire) > `BOT_LANGUAGE` (environnement) > `en`.
pub fn resolve_initial_lang(args: &[String]) -> &'static str {
    for a in args {
        if let Some(code) = a.strip_prefix("--lang=") {
            let code = code.trim().to_lowercase();
            if SUPPORTED.contains(&code.as_str()) {
                return match code.as_str() {
                    "fr" => "fr",
                    _ => "en",
                };
            }
            eprintln!(
                "[i18n] Unknown --lang value \"{code}\", falling back to \"{DEFAULT_LANG}\"."
            );
        }
    }
    let env = std::env::var("BOT_LANGUAGE")
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if SUPPORTED.contains(&env.as_str()) {
        return match env.as_str() {
            "fr" => "fr",
            _ => "en",
        };
    }
    if !env.is_empty() {
        eprintln!("[i18n] Unknown BOT_LANGUAGE \"{env}\", falling back to \"{DEFAULT_LANG}\".");
    }
    DEFAULT_LANG
}

/// Catalogue actif pour une langue (validée) — une simple référence statique.
pub fn catalog(lang: &str) -> &'static Catalog {
    match lang {
        "fr" => &fr::CATALOG,
        _ => &en::CATALOG,
    }
}

/// Interpolation d'un gabarit : remplace chaque `{nom}` par la valeur
/// associée. Les clés inconnues restent telles quelles (comportement du JS).
///
/// Les valeurs sont des `&str` : les appelants formatent leurs nombres au
/// préalable (`&seconds.to_string()`). Simple, prévisible, zéro allocation
/// cachée — dans l'esprit du Rust : on voit le coût.
pub fn fill(template: &str, params: &[(&str, &str)]) -> String {
    if params.is_empty() {
        return template.to_string();
    }
    let mut out = template.to_string();
    for (key, value) in params {
        out = out.replace(&format!("{{{key}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_replaces_known_params_only() {
        assert_eq!(
            fill("Hello {name}, {missing} stays", &[("name", "Apach")]),
            "Hello Apach, {missing} stays"
        );
    }

    #[test]
    fn fill_handles_multiple_occurrences() {
        assert_eq!(
            fill("{a} and {a}", &[("a", "x")]),
            "x and x"
        );
    }

    #[test]
    fn both_catalogs_are_compiled_in() {
        assert!(en::CATALOG.status.state_running.contains("ONLINE"));
        assert!(fr::CATALOG.status.state_running.contains("LIGNE"));
    }

    #[test]
    fn resolve_lang_prefers_cli_flag() {
        assert_eq!(
            resolve_initial_lang(&["bot".into(), "--lang=fr".into()]),
            "fr"
        );
        assert_eq!(
            resolve_initial_lang(&["bot".into(), "--lang=zz".into()]),
            DEFAULT_LANG
        );
    }
}
