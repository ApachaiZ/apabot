//! Provider Hetzner Cloud (API v1, Bearer token) — VPS.
//!
//! Endpoints :
//!   GET  /servers/{id}                     → `{server: {status, name, …}}`
//!   POST /servers/{id}/actions/poweron     → démarrer
//!   POST /servers/{id}/actions/poweroff    → éteindre
//!   POST /servers/{id}/actions/reboot      → redémarrer
//!
//! L'API n'expose pas les métriques d'usage en direct : CPU/RAM/disque/
//! uptime/joueurs restent `None` (le dashboard masque ces lignes).

use serde_json::Value;

use super::{map_state, str_at, Auth, Meta, Provider, Rest, Status};
use crate::errors::ProviderError;
use std::time::Duration;

pub const META: Meta = Meta {
    name: "hetzner",
    display_name: "Hetzner Cloud",
    kind: "vps",
    icon: "☁️",
    default_api_url: "https://api.hetzner.cloud/v1",
};

const GET_TIMEOUT: Duration = Duration::from_secs(15);
const POWER_TIMEOUT: Duration = Duration::from_secs(15);

/// Statuts Hetzner → vocabulaire commun.
/// - `initializing` précède `starting` (serveur en cours de création) ;
/// - `deleting`/`rebuilding` impliquent une indisponibilité → `stopping`.
const STATE_MAP: [(&str, &str); 8] = [
    ("running", "running"),
    ("off", "stopped"),
    ("starting", "starting"),
    ("initializing", "starting"),
    ("stopping", "stopping"),
    ("deleting", "stopping"),
    ("rebuilding", "stopping"),
    ("unknown", "unknown"),
];

/// Action générique (contrat) → verbe d'action Hetzner.
const ACTION_MAP: [(&str, &str); 3] =
    [("start", "poweron"), ("stop", "poweroff"), ("restart", "reboot")];

#[derive(Debug, Clone)]
pub struct Hetzner {
    rest: Rest,
}

impl Hetzner {
    pub fn new(api_key: &str, api_url: Option<&str>) -> Self {
        Self {
            rest: Rest::new(
                api_url.unwrap_or(META.default_api_url).to_string(),
                Auth::Bearer(api_key.to_string()),
                super::get_timeout(GET_TIMEOUT),
                POWER_TIMEOUT,
            ),
        }
    }
}

impl Provider for Hetzner {
    fn meta(&self) -> &'static Meta {
        &META
    }

    fn rest(&self) -> &Rest {
        &self.rest
    }

    async fn fetch_raw(&self, target: &str) -> Result<Value, ProviderError> {
        // Réponse brute (enveloppe {server: …}) laissée telle quelle :
        // normalize() sait la déplier.
        self.rest.get_json(&format!("/servers/{target}")).await
    }

    async fn send_power_raw(&self, target: &str, action: &str) -> Result<(), ProviderError> {
        let verb = ACTION_MAP
            .iter()
            .find(|(from, _)| *from == action)
            .map(|(_, to)| *to)
            .ok_or_else(|| ProviderError::other(format!("action inconnue pour hetzner: \"{action}\"")))?;
        self.rest
            .post_empty(&format!("/servers/{target}/actions/{verb}"))
            .await
            .map(|_| ())
    }

    fn normalize(&self, raw: &Value) -> Status {
        normalize(raw)
    }
}

pub fn normalize(raw: &Value) -> Status {
    // L'API renvoie {server: {...}} ; on accepte aussi un objet déjà déplié.
    let server = raw.get("server").unwrap_or(raw);
    let raw_state = str_at(server, &["status"])
        .unwrap_or_else(|| "unknown".to_string())
        .to_lowercase();
    Status {
        name: str_at(server, &["name"]),
        state: map_state(&raw_state, &STATE_MAP, "unknown"),
        raw_state,
        cpu_pct: None,
        ram_mb: None,
        ram_max_mb: None,
        disk_mb: None,
        disk_max_mb: None,
        uptime_seconds: None,
        players: None,
        address: str_at(server, &["public_net", "ipv4", "ip"])
            .map(|ip| super::Address { ip, port: None }),
        node: str_at(server, &["datacenter", "name"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_hetzner_statuses() {
        let norm = |raw: Value| Hetzner::new("k", None).normalize(&raw);
        assert_eq!(norm(json!({"server": {"status": "running"}})).state, "running");
        assert_eq!(norm(json!({"server": {"status": "off"}})).state, "stopped");
        assert_eq!(norm(json!({"server": {"status": "initializing"}})).state, "starting");
        assert_eq!(norm(json!({"server": {"status": "rebuilding"}})).state, "stopping");
        assert_eq!(norm(json!({"server": {"status": "migrating"}})).state, "unknown");
    }

    #[test]
    fn extracts_public_ip_and_datacenter() {
        let s = Hetzner::new("k", None).normalize(&json!({
            "server": {
                "status": "running",
                "name": "mon-vps",
                "public_net": {"ipv4": {"ip": "203.0.113.7"}},
                "datacenter": {"name": "fsn1-dc14"}
            }
        }));
        assert_eq!(s.address.unwrap().ip, "203.0.113.7");
        assert_eq!(s.node.as_deref(), Some("fsn1-dc14"));
    }

    #[test]
    fn maps_power_actions() {
        // Le mapping est une table : vérifions son exhaustivité.
        for action in ["start", "stop", "restart"] {
            assert!(ACTION_MAP.iter().any(|(from, _)| *from == action));
        }
    }
}
