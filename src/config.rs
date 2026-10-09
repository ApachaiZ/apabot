//! Chargement et validation de la configuration (`.config.d/.env` > environnement).
//!
//! Le bot lit `.config.d/.env` en priorité, puis les variables d'environnement
//! système si une valeur est absente (déploiement systemd/Docker). Toute
//! valeur manquante ou placeholder (`YOUR_…`) est une erreur de
//! CONFIGURATION → sortie avec le code 78 (EX_CONFIG) : le superviseur
//! embarqué (et systemd) ne redémarrent PAS en boucle sur une config
//! invalide.

use std::sync::OnceLock;

use crate::errors::ExitError;
use crate::i18n::{fill, Catalog};
use crate::paths;
use crate::providers::{self, AnyProvider, Provider};
use crate::services::{is_unset, parse_services};

/// Lecture d'une variable d'environnement (le `.env` est déjà chargé par
/// dotenvy au démarrage, voir [`load_dotenv`]).
fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_string())
}

/// Charge `.config.d/.env` UNE seule fois (dotenvy n'écrase jamais les vraies
/// variables d'environnement : mêmes priorités que dotenv en JS).
pub fn load_dotenv() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = dotenvy::from_path(paths::env_file());
    });
}

/// La configuration figée du bot (immuable une fois construite).
#[derive(Clone)]
pub struct Config {
    pub token: String,
    pub client_id: String,
    pub owner_id: String,
    pub guild_id: String,
    pub alert_channel_id: Option<String>,
    pub alert_interval_ms: Option<u64>,
    /// Libellé affiché du provider (cartes, alertes watchdog).
    pub provider_label: &'static str,
    /// Services `(alias, id)` dans l'ordre de déclaration — le premier est
    /// le service par défaut.
    pub services: Vec<(String, String)>,
    pub language: &'static str,
}

impl Config {
    /// Catalogue i18n associé à la langue de la config — pratique pour les
    /// tâches détachées (watchdog) qui ne reçoivent pas le catalogue.
    pub fn language_catalog(&self) -> &'static Catalog {
        crate::i18n::catalog(self.language)
    }
}

/// Charge la configuration et construit le provider. Retourne le couple
/// (config, provider) — le provider est détaché car il vit dans la couche
/// API, pas dans la config.
pub fn load(
    catalog: &'static Catalog,
    language: &'static str,
) -> Result<(Config, AnyProvider), ExitError> {
    load_dotenv();

    let provider_name = env("PROVIDER").unwrap_or_else(|| "yorkhost".to_string());
    let api_key = env("PROVIDER_API_KEY").unwrap_or_default();
    let api_url_override = env("PROVIDER_API_URL");

    // Le provider est construit AVANT la validation : ses métadonnées
    // (libellé, URL par défaut) entrent dans la config. Un nom de provider
    // inconnu ou des secrets OVH manquants → erreur de configuration.
    let provider = providers::AnyProvider::build(&provider_name, &api_key, api_url_override.as_deref())?;
    let meta = provider.meta();

    let services = parse_services(
        env("PROVIDER_SERVICES").as_deref(),
        env("PROVIDER_SERVICE_ID").as_deref(),
    );

    // ── Validation bloquante (messages localisés, exit 78) ──
    for (value, key) in [
        (env("DISCORD_TOKEN"), "DISCORD_TOKEN"),
        (env("DISCORD_CLIENT_ID"), "DISCORD_CLIENT_ID"),
        (env("DISCORD_OWNER_ID"), "DISCORD_OWNER_ID"),
        (env("DISCORD_GUILD_ID"), "DISCORD_GUILD_ID"),
    ] {
        if is_unset(value.as_deref()) {
            return Err(ExitError::config(fill(
                catalog.errors.missing_config,
                &[("key", key)],
            )));
        }
    }
    if is_unset(Some(&api_key)) {
        return Err(ExitError::config(fill(
            catalog.errors.missing_config,
            &[("key", "PROVIDER_API_KEY")],
        )));
    }
    if services.is_empty() {
        return Err(ExitError::config(catalog.errors.invalid_service));
    }
    for (alias, id) in &services {
        if is_unset(Some(id)) {
            return Err(ExitError::config(fill(
                catalog.errors.missing_service_id,
                &[("alias", alias)],
            )));
        }
    }

    let cfg = Config {
        token: env("DISCORD_TOKEN").unwrap_or_default(),
        client_id: env("DISCORD_CLIENT_ID").unwrap_or_default(),
        owner_id: env("DISCORD_OWNER_ID").unwrap_or_default(),
        guild_id: env("DISCORD_GUILD_ID").unwrap_or_default(),
        alert_channel_id: env("DISCORD_ALERT_CHANNEL_ID"),
        // Un intervalle invalide (NaN) retombe sur le défaut côté watchdog,
        // comme `Number(...)` > 0 le faisait en JS.
        alert_interval_ms: env("ALERT_INTERVAL_MS").and_then(|v| v.parse::<u64>().ok()),
        provider_label: meta.display_name,
        services,
        language,
    };
    Ok((cfg, provider))
}
