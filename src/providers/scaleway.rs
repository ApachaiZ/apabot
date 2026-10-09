//! Provider Scaleway (Instance API v1, secret key).
//!
//! Authentification : header `X-Auth-Token: <secret key>`.
//! La ZONE est obligatoire dans l'URL de l'Instance API :
//! `/instance/v1/zones/{zone}/servers/…` — lue depuis `SCW_ZONE`
//! (défaut `fr-par-1`), intégrée à la base URL.
//!
//!   GET  /instance/v1/zones/{zone}/servers/{id}          → état
//!   POST /instance/v1/zones/{zone}/servers/{id}/action   → `{"action": "poweron"|"poweroff"|"reboot"}`

use serde_json::{json, Value};

use super::{str_at, Auth, Meta, Provider, Rest, Status};
use crate::errors::ProviderError;
use std::time::Duration;

pub const META: Meta = Meta {
    name: "scaleway",
    display_name: "Scaleway",
    kind: "vps",
    icon: "☁️",
    default_api_url: "https://api.scaleway.com",
};

const GET_TIMEOUT: Duration = Duration::from_secs(15);
const POWER_TIMEOUT: Duration = Duration::from_secs(15);

/// Actions utilisateur (vocabulaire du bot) → actions API Scaleway.
const POWER_ACTIONS: [(&str, &str); 3] =
    [("start", "poweron"), ("stop", "poweroff"), ("restart", "reboot")];

/// États déjà alignés sur le vocabulaire commun ; tout autre état
/// (`locked` : snapshot/maintenance en cours…) → `unknown`.
const KNOWN_STATES: [&str; 4] = ["running", "stopped", "starting", "stopping"];

#[derive(Debug, Clone)]
pub struct Scaleway {
    rest: Rest,
}

impl Scaleway {
    pub fn new(api_key: &str, api_url: Option<&str>) -> Self {
        // La zone est lue ICI (jamais ailleurs) : `createClient` du JS lisait
        // `SCW_ZONE` au même moment.
        let zone = std::env::var("SCW_ZONE")
            .ok()
            .filter(|z| !z.trim().is_empty())
            .unwrap_or_else(|| "fr-par-1".to_string());
        let base = api_url.unwrap_or(META.default_api_url);
        Self {
            rest: Rest::new(
                format!("{}/instance/v1/zones/{}", base.trim_end_matches('/'), zone),
                Auth::XAuthToken(api_key.to_string()),
                super::get_timeout(GET_TIMEOUT),
                POWER_TIMEOUT,
            ),
        }
    }
}

impl Provider for Scaleway {
    fn meta(&self) -> &'static Meta {
        &META
    }

    fn rest(&self) -> &Rest {
        &self.rest
    }

    async fn fetch_raw(&self, target: &str) -> Result<Value, ProviderError> {
        self.rest.get_json(&format!("/servers/{target}")).await
    }

    async fn send_power_raw(&self, target: &str, action: &str) -> Result<(), ProviderError> {
        let api_action = POWER_ACTIONS
            .iter()
            .find(|(from, _)| *from == action)
            .map(|(_, to)| *to)
            .ok_or_else(|| {
                ProviderError::other(format!(
                    "scaleway: action inconnue \"{action}\" (attendu : start|stop|restart)"
                ))
            })?;
        self.rest
            .post_json(
                &format!("/servers/{target}/action"),
                &json!({ "action": api_action }),
            )
            .await
            .map(|_| ())
    }

    fn normalize(&self, raw: &Value) -> Status {
        normalize(raw)
    }
}

pub fn normalize(raw: &Value) -> Status {
    let raw_state = str_at(raw, &["state"])
        .unwrap_or_else(|| "unknown".to_string())
        .to_lowercase();
    let state = if KNOWN_STATES.contains(&raw_state.as_str()) {
        raw_state.clone()
    } else {
        "unknown".to_string()
    };
    Status {
        name: str_at(raw, &["name"]),
        state,
        raw_state,
        cpu_pct: None,
        ram_mb: None,
        ram_max_mb: None,
        disk_mb: None,
        disk_max_mb: None,
        uptime_seconds: None,
        players: None,
        address: str_at(raw, &["public_ip", "address"])
            .map(|ip| super::Address { ip, port: None }),
        node: str_at(raw, &["zone"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn known_states_pass_through_unknown_collapses() {
        let norm = |raw: Value| Scaleway::new("k", None).normalize(&raw);
        assert_eq!(norm(json!({"state": "running"})).state, "running");
        assert_eq!(norm(json!({"state": "stopping"})).state, "stopping");
        assert_eq!(norm(json!({"state": "locked"})).state, "unknown");
    }

    #[test]
    fn zone_is_injected_in_base_url() {
        // La zone est intégrée à la baseURL (obligatoire côté Instance API).
        let p = Scaleway::new("k", None);
        assert_eq!(p.rest().base_url_for_test(), "https://api.scaleway.com/instance/v1/zones/fr-par-1");
    }
}
