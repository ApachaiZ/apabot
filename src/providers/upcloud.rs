//! Provider UpCloud (API REST 1.3, HTTP Basic).
//!
//! Authentification : la variable `PROVIDER_API_KEY` contient les
//! identifiants d'un sous-compte API au format `user:password`, encodés en
//! base64 (`Authorization: Basic …`).
//!
//!   GET  /1.3/server/{uuid}          → détails (champ `state`)
//!   POST /1.3/server/{uuid}/start    → démarrage (corps vide)
//!   POST /1.3/server/{uuid}/stop     → arrêt (corps vide)
//!   POST /1.3/server/{uuid}/restart  → redémarrage (corps vide)
//!
//! États : `started` → running, `stopped` → stopped ; `maintenance` et
//! `error` → unknown (le serveur n'est alors ni réellement démarré ni
//! réellement arrêté).

use serde_json::Value;

use super::{str_at, Auth, Meta, Provider, Rest, Status};
use crate::errors::ProviderError;
use std::time::Duration;

pub const META: Meta = Meta {
    name: "upcloud",
    display_name: "UpCloud",
    kind: "vps",
    icon: "☁️",
    default_api_url: "https://api.upcloud.com",
};

const GET_TIMEOUT: Duration = Duration::from_secs(15);
const POWER_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct Upcloud {
    rest: Rest,
}

impl Upcloud {
    pub fn new(api_key: &str, api_url: Option<&str>) -> Self {
        Self {
            rest: Rest::new(
                api_url.unwrap_or(META.default_api_url).to_string(),
                Auth::Basic(api_key.to_string()),
                super::get_timeout(GET_TIMEOUT),
                POWER_TIMEOUT,
            ),
        }
    }
}

impl Provider for Upcloud {
    fn meta(&self) -> &'static Meta {
        &META
    }

    fn rest(&self) -> &Rest {
        &self.rest
    }

    async fn fetch_raw(&self, target: &str) -> Result<Value, ProviderError> {
        self.rest.get_json(&format!("/1.3/server/{target}")).await
    }

    async fn send_power_raw(&self, target: &str, action: &str) -> Result<(), ProviderError> {
        if !["start", "stop", "restart"].contains(&action) {
            return Err(ProviderError::other(format!("action d'alimentation inconnue pour UpCloud : \"{action}\"")));
        }
        self.rest
            .post_empty(&format!("/1.3/server/{target}/{action}"))
            .await
            .map(|_| ())
    }

    fn normalize(&self, raw: &Value) -> Status {
        normalize(raw)
    }
}

pub fn normalize(raw: &Value) -> Status {
    // Accepte la réponse enveloppée {server: {…}} comme un objet direct.
    let server = raw.get("server").unwrap_or(raw);
    let raw_state = str_at(server, &["state"])
        .unwrap_or_else(|| "unknown".to_string())
        .to_lowercase();
    let state = match raw_state.as_str() {
        "started" => "running".to_string(),
        "stopped" => "stopped".to_string(),
        _ => "unknown".to_string(),
    };
    Status {
        name: str_at(server, &["title"]).or_else(|| str_at(server, &["hostname"])),
        state,
        raw_state,
        cpu_pct: None,
        ram_mb: None,
        ram_max_mb: None,
        disk_mb: None,
        disk_max_mb: None,
        uptime_seconds: None,
        players: None,
        // Formes rencontrées : {ip_addresses: {ip_address: [...]}} (détails)
        // ou {ip_addresses: [...]} (réponses start/stop). On retient la
        // première IP publique.
        address: server
            .get("ip_addresses")
            .and_then(|ips| ips.get("ip_address").or(Some(ips)))
            .and_then(|list| list.as_array())
            .and_then(|list| {
                list.iter().find_map(|entry| {
                    if entry.get("access").and_then(|a| a.as_str()) == Some("public") {
                        str_at(entry, &["address"]).map(|ip| super::Address { ip, port: None })
                    } else {
                        None
                    }
                })
            }),
        node: str_at(server, &["zone"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_states() {
        let norm = |raw: Value| Upcloud::new("user:pass", None).normalize(&raw);
        assert_eq!(norm(json!({"server": {"state": "started"}})).state, "running");
        assert_eq!(norm(json!({"server": {"state": "stopped"}})).state, "stopped");
        assert_eq!(norm(json!({"server": {"state": "maintenance"}})).state, "unknown");
        assert_eq!(norm(json!({"server": {"state": "error"}})).state, "unknown");
    }

    #[test]
    fn extracts_public_ip_from_nested_form() {
        let s = Upcloud::new("user:pass", None).normalize(&json!({
            "server": {
                "state": "started",
                "title": "srv-1",
                "ip_addresses": {"ip_address": [
                    {"access": "private", "address": "10.0.0.2"},
                    {"access": "public", "address": "203.0.113.5"}
                ]}
            }
        }));
        assert_eq!(s.address.unwrap().ip, "203.0.113.5");
    }
}
