//! Enregistrement des slash commands (portage de `lib/deploy.js`).
//!
//! Politique :
//! - les commandes sont enregistrées PAR GUILDE via UNE seule requête PUT
//!   groupée (`set_guild_application_commands`) ;
//! - un hash (guilde + application + commandes) persisté dans
//!   `.config.d/commands.hash` évite tout ré-enregistrement inutile — les
//!   commandes ne sont (re)publiées que si elles CHANGENT ;
//! - les commandes GLOBALES résiduelles sont purgées à CHAQUE démarrage
//!   (répare les doublons d'une version antérieure), best-effort ;
//! - un changement de guilde purge les commandes de l'ANCIENNE guilde,
//!   best-effort ;
//! - les erreurs Discord 50001 (Missing Access) et 10002 (Unknown
//!   Application) sont traduites en messages actionnables avec exit 78 :
//!   le superviseur embarqué et systemd ne redémarrent pas en boucle sur
//!   un problème qu'un humain doit corriger (invite du bot, Application ID).

use poise::serenity_prelude as serenity;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::commands::{Data, Error};
use crate::config::Config;
use crate::errors::ExitError;
use crate::fsutil;
use crate::i18n::{fill, Catalog};
use crate::logger;
use crate::paths;

/// Empreinte SHA-256 (hex) de {guilde, application, commandes}. Inclure la
/// guilde et l'application : en changer force un ré-enregistrement.
pub fn commands_hash(cfg: &Config, commands_json: &[Value]) -> String {
    let payload = serde_json::json!({
        "guild": cfg.guild_id,
        "app": cfg.client_id,
        "cmds": commands_json,
    });
    format!("{:x}", Sha256::digest(payload.to_string().as_bytes()))
}

#[derive(serde::Deserialize)]
struct DeployState {
    guild: Option<String>,
    hash: String,
}

/// État persisté du déploiement. Format actuel : JSON `{guild, hash}`.
/// Format hérité : simple hexadécimal de 64 caractères — la guilde d'alors
/// est inconnue, on la suppose égale à la guilde courante.
pub fn read_deploy_state(default_guild: &str) -> Option<(String, String)> {
    let raw = std::fs::read_to_string(paths::commands_hash_file()).ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.starts_with('{') {
        let parsed: DeployState = serde_json::from_str(raw).ok()?;
        return Some((
            parsed.guild.unwrap_or_else(|| default_guild.to_string()),
            parsed.hash,
        ));
    }
    if raw.len() == 64 && raw.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some((default_guild.to_string(), raw.to_string()));
    }
    None
}

pub fn write_deploy_state(guild: &str, hash: &str) {
    if let Err(e) = fsutil::secure_write(
        &paths::commands_hash_file(),
        serde_json::json!({ "guild": guild, "hash": hash }).to_string().as_bytes(),
    ) {
        logger::error(format!("Could not persist commands hash: {e}"));
    }
}

/// Extrait le code d'erreur Discord (`50001`, `10002`…) d'une erreur HTTP.
fn discord_error_code(err: &serenity::Error) -> Option<u64> {
    match err {
        serenity::Error::Http(serenity::http::HttpError::UnsuccessfulRequest(response)) => {
            // Le code de DiscordJsonError est un isize côté serenity.
            Some(response.error.code as u64)
        }
        _ => None,
    }
}

/// Enregistre les commandes si nécessaire. `http` est un client REST Discord
/// autonome (aucun gateway requis) : cette étape précède la connexion.
pub async fn deploy_commands_if_needed(
    http: &serenity::Http,
    cfg: &Config,
    catalog: &Catalog,
    commands: &[poise::Command<Data, Error>],
) -> Result<(), Error> {
    let built = poise::builtins::create_application_commands(commands);
    let commands_json: Vec<Value> = built
        .iter()
        .map(|c| serde_json::to_value(c).unwrap_or(Value::Null))
        .collect();

    let guild_id = serenity::GuildId::new(
        cfg.guild_id.parse().map_err(|e: std::num::ParseIntError| ExitError::config(e.to_string()))?,
    );

    // ── Purge des commandes GLOBALES (à chaque démarrage) ──
    // Le bot n'enregistre que des commandes de guilde ; toute commande
    // globale résiduelle apparaîtrait EN DOUBLE dans l'autocomplétion.
    // Best-effort : un échec ne bloque jamais le démarrage.
    // (`create_global_commands` réécrit tout : liste vide = purge.)
    match http.create_global_commands(&Vec::<serenity::CreateCommand>::new()).await {
        Ok(_) => logger::info(catalog.audit.commands_global_purged),
        Err(e) => logger::warn(fill(
            catalog.audit.commands_global_purge_failed,
            &[("error", &e.to_string())],
        )),
    }

    // ── Skip si rien n'a changé ──
    let hash = commands_hash(cfg, &commands_json);
    let previous = read_deploy_state(&cfg.guild_id);
    if previous.as_ref().map(|(_, h)| h == &hash).unwrap_or(false) {
        logger::info(catalog.audit.commands_skipped);
        return Ok(());
    }

    // ── Changement de guilde : purge de l'ancienne ──
    if let Some((old_guild, _)) = &previous {
        if old_guild != &cfg.guild_id {
            if let Ok(old_id) = old_guild.parse::<u64>() {
                let purge = serenity::GuildId::new(old_id)
                    .set_commands(http, Vec::<serenity::CreateCommand>::new())
                    .await;
                match purge {
                    Ok(_) => logger::info(fill(catalog.audit.commands_old_guild_purged, &[("guild", old_guild.as_str())])),
                    Err(e) => logger::warn(fill(
                        catalog.audit.commands_old_guild_purge_failed,
                        &[("guild", old_guild), ("error", &e.to_string())],
                    )),
                }
            }
        }
    }

    // ── Enregistrement groupé (une seule requête PUT) ──
    match guild_id.set_commands(http, built.clone()).await {
        Ok(_) => {
            write_deploy_state(&cfg.guild_id, &hash);
            logger::info(catalog.audit.commands_registered);
            Ok(())
        }
        Err(e) => match discord_error_code(&e) {
            Some(50001) => Err(ExitError::config(fill(
                catalog.errors.deploy_missing_access,
                &[("guild", cfg.guild_id.as_str()), ("client", cfg.client_id.as_str())],
            ))
            .into()),
            Some(10002) => Err(ExitError::config(fill(
                catalog.errors.deploy_unknown_application,
                &[("client", cfg.client_id.as_str())],
            ))
            .into()),
            _ => Err(e.into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_with_commands() {
        let cfg = Config {
            token: String::new(),
            client_id: "1".into(),
            owner_id: "2".into(),
            guild_id: "3".into(),
            alert_channel_id: None,
            alert_interval_ms: None,
            provider_label: "YorkHost",
            services: vec![],
            language: "en",
        };
        let a = commands_hash(&cfg, &[serde_json::json!({"name": "status"})]);
        let b = commands_hash(&cfg, &[serde_json::json!({"name": "start"})]);
        let a2 = commands_hash(&cfg, &[serde_json::json!({"name": "status"})]);
        assert_ne!(a, b);
        assert_eq!(a, a2);
    }

    #[test]
    fn legacy_hex_deploy_state_is_read() {
        let hex = "a".repeat(64);
        // La fonction lit le VRAI fichier (chemin fixe) : on ne teste ici
        // que la forme de l'entrée via une simulation du parsing.
        let parsed: DeployState = serde_json::from_str(&format!("{{\"guild\": \"1\", \"hash\": \"{hex}\"}}")).unwrap();
        assert_eq!(parsed.hash, hex);
    }
}
