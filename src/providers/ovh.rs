//! Provider OVHcloud — instances VPS (API `/vps`, clés AK/AS/CK signées).
//!
//! Authentification (3 clés) :
//!   AK = clé application    → `PROVIDER_API_KEY`
//!   AS = secret application → env `OVH_APPLICATION_SECRET`
//!   CK = clé consommateur   → env `OVH_CONSUMER_KEY`
//!
//! Signature : `"$1$" + SHA1_HEX(AS+"+"+CK+"+"+METHOD+"+"+QUERY+"+"+BODY+"+"+TSTAMP)`,
//! envoyée via les en-têtes `X-Ovh-Application`, `X-Ovh-Consumer`,
//! `X-Ovh-Timestamp` et `X-Ovh-Signature`. La signature est appliquée à
//! CHAQUE requête par la plomberie commune [`Rest`] (voir `Auth::Ovh`).
//!
//! Endpoints :
//!   GET  /vps/{serviceName}        → informations du VPS (champ `state`)
//!   POST /vps/{serviceName}/start  → démarrer
//!   POST /vps/{serviceName}/stop   → arrêter
//!   POST /vps/{serviceName}/reboot → redémarrer
//!
//! Normalisation : `running` → running ; `stopped` → stopped ; `rebooting` →
//! stopping (le VPS s'arrête avant de redémarrer) ; `rescued` → unknown
//! (système de secours : les services réels ne sont pas actifs) ; tout autre
//! état (`installed`, `suspended`, `updating`…) → unknown.

use serde_json::Value;
use sha1::{Digest, Sha1};

use super::{map_state, str_at, Auth, Meta, OvhKeys, Provider, Rest, Status};
use crate::errors::{ExitError, ProviderError};
use std::time::Duration;

pub const META: Meta = Meta {
    name: "ovh",
    display_name: "OVHcloud",
    kind: "vps",
    icon: "☁️",
    default_api_url: "https://eu.api.ovh.com/v1",
};

const GET_TIMEOUT: Duration = Duration::from_secs(15);
const POWER_TIMEOUT: Duration = Duration::from_secs(15);

const STATE_MAP: [(&str, &str); 3] =
    [("running", "running"), ("stopped", "stopped"), ("rebooting", "stopping")];

/// Action générique (contrat) → endpoint OVH.
const POWER_ACTIONS: [(&str, &str); 3] =
    [("start", "start"), ("stop", "stop"), ("restart", "reboot")];

#[derive(Debug, Clone)]
pub struct Ovh {
    rest: Rest,
}

impl Ovh {
    /// Les secrets complémentaires sont lus ICI (jamais ailleurs), comme
    /// `createClient` le faisait dans le JS. Leur absence est une erreur de
    /// CONFIGURATION (exit 78) : sans eux, l'API renverrait un 401 confus.
    pub fn new(api_key: &str, api_url: Option<&str>) -> Result<Self, ExitError> {
        let app_secret = std::env::var("OVH_APPLICATION_SECRET")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let consumer_key = std::env::var("OVH_CONSUMER_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let (Some(app_secret), Some(consumer_key)) = (app_secret, consumer_key) else {
            return Err(ExitError::config(
                "Provider ovh : OVH_APPLICATION_SECRET et OVH_CONSUMER_KEY doivent être définis (voir README → Identifiants spécifiques par provider).",
            ));
        };
        Ok(Self::with_keys(api_key, api_url, &app_secret, &consumer_key))
    }

    /// Constructeur direct à clés explicites — utilisé par les tests pour
    /// ne pas dépendre de l'environnement du process.
    pub fn with_keys(api_key: &str, api_url: Option<&str>, app_secret: &str, consumer_key: &str) -> Self {
        Self {
            rest: Rest::new(
                api_url.unwrap_or(META.default_api_url).to_string(),
                Auth::Ovh(OvhKeys {
                    app_key: api_key.to_string(),
                    app_secret: app_secret.to_string(),
                    consumer_key: consumer_key.to_string(),
                }),
                super::get_timeout(GET_TIMEOUT),
                POWER_TIMEOUT,
            ),
        }
    }
}

impl Provider for Ovh {
    fn meta(&self) -> &'static Meta {
        &META
    }

    fn rest(&self) -> &Rest {
        &self.rest
    }

    async fn fetch_raw(&self, target: &str) -> Result<Value, ProviderError> {
        self.rest.get_json(&format!("/vps/{target}")).await
    }

    async fn send_power_raw(&self, target: &str, action: &str) -> Result<(), ProviderError> {
        let endpoint = POWER_ACTIONS
            .iter()
            .find(|(from, _)| *from == action)
            .map(|(_, to)| *to)
            .ok_or_else(|| ProviderError::other(format!("Action d'alimentation inconnue : \"{action}\"")))?;
        self.rest.post_empty(&format!("/vps/{target}/{endpoint}")).await.map(|_| ())
    }

    fn normalize(&self, raw: &Value) -> Status {
        normalize(raw)
    }
}

pub fn normalize(raw: &Value) -> Status {
    let raw_state = str_at(raw, &["state"])
        .unwrap_or_else(|| "unknown".to_string())
        .to_lowercase();
    Status {
        // L'API expose `displayName` (nom choisi par l'utilisateur) et
        // `model` : le premier prime.
        name: str_at(raw, &["displayName"]).or_else(|| str_at(raw, &["model"])),
        state: map_state(&raw_state, &STATE_MAP, "unknown"),
        raw_state,
        cpu_pct: None,
        ram_mb: None,
        ram_max_mb: None,
        disk_mb: None,
        disk_max_mb: None,
        uptime_seconds: None,
        players: None,
        // L'IP n'est pas dans GET /vps/{serviceName} (endpoint séparé /ips,
        // non appelé pour ne pas multiplier les requêtes — comme en JS).
        address: None,
        node: str_at(raw, &["zone"]),
    }
}

/// Signature OVHcloud d'une requête — extraite en fonction PUBLIQUE et
/// PURE pour être testable sans réseau ni environnement.
#[allow(dead_code)] // utilisée par les tests (et documente le format "$1$")
pub fn sign(method: &str, path: &str, body: &str, app_secret: &str, consumer_key: &str, timestamp: u64) -> String {
    let to_sign = format!(
        "{}+{}+{}+{}+{}+{}",
        app_secret,
        consumer_key,
        method.to_uppercase(),
        path,
        body,
        timestamp
    );
    format!("$1${:x}", Sha1::digest(to_sign.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sign_matches_documented_format() {
        // Vecteur de contrôle : SHA-1 hex (minuscules) de
        // "AS+CK+GET+/vps/abc123++1700000000", préfixé "$1$" — valeur
        // calculée indépendamment de la fonction testée (sha1sum).
        let sig = sign("GET", "/vps/abc123", "", "AS", "CK", 1700000000);
        assert_eq!(sig, "$1$73d5e22020693bfa41b183831ce4977a8b200e5b");
    }

    #[test]
    fn maps_ovh_states() {
        let norm = |raw: Value| Ovh::with_keys("k", None, "AS", "CK").normalize(&raw);
        assert_eq!(norm(json!({"state": "running"})).state, "running");
        assert_eq!(norm(json!({"state": "rebooting"})).state, "stopping");
        assert_eq!(norm(json!({"state": "rescued"})).state, "unknown");
        assert_eq!(norm(json!({"state": "suspended"})).state, "unknown");
    }

    #[test]
    fn prefers_display_name_over_model() {
        let s = Ovh::with_keys("k", None, "AS", "CK").normalize(&json!({"state": "stopped", "displayName": "Mon VPS", "model": "vps-essential"}));
        assert_eq!(s.name.as_deref(), Some("Mon VPS"));
    }
}
