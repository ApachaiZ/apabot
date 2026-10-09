//! Provider YorkHost (API v1, Bearer) — serveurs de jeu. Provider de
//! RÉFÉRENCE du contrat : lecture d'état + POST power + normalisation
//! complète (CPU/RAM/disque/uptime/joueurs fournis par l'API).

use serde_json::{json, Value};

use super::{num, normalize_address, str_at, Auth, Meta, Provider, Rest, Status};
use crate::errors::ProviderError;
use std::time::Duration;

pub const META: Meta = Meta {
    name: "yorkhost",
    display_name: "YorkHost",
    kind: "game",
    icon: "🎮",
    default_api_url: "https://api.yorkhost.fr/client/v1",
};

// Délais distincts, constatés en production :
// - lectures : 60 s — l'API a des blocages intermittents (12 s observés,
//   confirmation de restart à 78 s) ; 30 s produisait des faux positifs au
//   watchdog. Surchargeable par PROVIDER_GET_TIMEOUT_MS ;
// - POST /power stop : 120 s — l'API ne répond qu'une fois l'arrêt COMPLET
//   du jeu effectué (ApaWorld/Zomboid > 15 s). Le start répond immédiatement.
const GET_TIMEOUT: Duration = Duration::from_secs(60);
const POWER_TIMEOUT: Duration = Duration::from_secs(120);

const ONLINE: [&str; 2] = ["running", "online"];
const OFFLINE: [&str; 2] = ["offline", "stopped"];

#[derive(Debug, Clone)]
pub struct Yorkhost {
    rest: Rest,
}

impl Yorkhost {
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

impl Provider for Yorkhost {
    fn meta(&self) -> &'static Meta {
        &META
    }

    fn rest(&self) -> &Rest {
        &self.rest
    }

    async fn fetch_raw(&self, target: &str) -> Result<Value, ProviderError> {
        self.rest.get_json(&format!("/game-servers/{target}")).await
    }

    async fn send_power_raw(&self, target: &str, action: &str) -> Result<(), ProviderError> {
        if !["start", "stop", "restart"].contains(&action) {
            return Err(ProviderError::other(format!("action inconnue pour yorkhost: \"{action}\"")));
        }
        self.rest
            .post_json(&format!("/game-servers/{target}/power"), &json!({ "action": action }))
            .await
            .map(|_| ())
    }

    fn normalize(&self, raw: &Value) -> Status {
        normalize(raw)
    }
}

/// Normalisation vers le contrat canonique. Les champs `players` et
/// `uptimeSeconds` peuvent être `null` même serveur running (constaté sur
/// « ApaWorld ») : le contrat tolère `None` (dashboard masque, l'uptime
/// reste utilisé en interne par la détection de redémarrage).
pub fn normalize(raw: &Value) -> Status {
    let raw_state = str_at(raw, &["state"])
        .unwrap_or_else(|| "unknown".to_string())
        .to_lowercase();
    let state = if ONLINE.contains(&raw_state.as_str()) {
        "running".to_string()
    } else if OFFLINE.contains(&raw_state.as_str()) {
        "stopped".to_string()
    } else {
        raw_state.clone()
    };
    Status {
        name: str_at(raw, &["name"]),
        state,
        raw_state,
        cpu_pct: num(raw, &["cpuPct"]),
        ram_mb: num(raw, &["ramMb"]),
        ram_max_mb: num(raw, &["ramMaxMb"]),
        disk_mb: num(raw, &["diskMb"]),
        disk_max_mb: num(raw, &["diskMaxMb"]),
        uptime_seconds: num(raw, &["uptimeSeconds"]),
        players: num(raw, &["players"]),
        address: raw.get("address").and_then(normalize_address),
        node: str_at(raw, &["node"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn status(raw: Value) -> Status {
        Yorkhost::new("k", None).normalize(&raw)
    }

    #[test]
    fn maps_states_to_common_vocabulary() {
        assert_eq!(status(json!({"state": "running"})).state, "running");
        assert_eq!(status(json!({"state": "online"})).state, "running");
        assert_eq!(status(json!({"state": "stopped"})).state, "stopped");
        assert_eq!(status(json!({"state": "offline"})).state, "stopped");
        // État inconnu : conservé brut.
        assert_eq!(status(json!({"state": "rebooting"})).state, "rebooting");
        assert_eq!(status(json!({})).state, "unknown");
    }

    #[test]
    fn normalizes_address_forms() {
        let a = status(json!({"address": "play.example.fr:25565"})).address.unwrap();
        assert_eq!(a.ip, "play.example.fr");
        assert_eq!(a.port, Some(25565));
        let a = status(json!({"address": {"host": "h", "port": "80"}})).address.unwrap();
        assert_eq!(a.ip, "h");
        assert_eq!(a.port, Some(80));
        let a = status(json!({"address": "1.2.3.4"})).address.unwrap();
        assert_eq!(a.port, None);
    }

    #[test]
    fn tolerates_null_metrics() {
        let s = status(json!({"state": "running", "players": null, "uptimeSeconds": null}));
        assert_eq!(s.state, "running");
        assert!(s.players.is_none());
        assert!(s.uptime_seconds.is_none());
    }
}
