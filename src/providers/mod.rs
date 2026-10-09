//! Providers pluggables : adaptateurs REST vers les hébergeurs.
//!
//! Traduction en Rust du contrat `lib/providers/base.js` : chaque provider
//! expose la même interface au reste du bot. En Rust, le contrat n'est plus
//! un `validate()` au chargement mais un **trait** ([`Provider`]) — toute
//! implémentation incomplète est une erreur de compilation, et le
//! vocabulaire d'état canonique est garanti par le type [`Status`].
//!
//! Factorisation : la plomberie HTTP commune (client reqwest, authentification,
//! timeouts, mapping d'erreurs) vit dans [`Rest`] ; chaque module de provider
//! ne déclare plus que ses endpoints, sa normalisation et ses constantes.

pub mod digitalocean;
pub mod generic;
pub mod hetzner;
pub mod nitrado;
pub mod ovh;
pub mod scaleway;
pub mod upcloud;
pub mod vultr;
pub mod yorkhost;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::Sha1;
use sha1::Digest as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::errors::ProviderError;

// ── État canonique ────────────────────────────────────────────────────────

/// Adresse de connexion normalisée (`{ip, port}`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Address {
    pub ip: String,
    pub port: Option<u32>,
}

/// État canonique d'un service. Le champ `state` est TOUJOURS ramené au
/// vocabulaire commun : `running` | `stopped` | `starting` | `stopping` |
/// `unknown`. Les champs de métriques valent `None` quand le provider ne
/// les fournit pas (le dashboard masque alors les lignes correspondantes).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Status {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub raw_state: String,
    #[serde(default)]
    pub cpu_pct: Option<f64>,
    #[serde(default)]
    pub ram_mb: Option<f64>,
    #[serde(default)]
    pub ram_max_mb: Option<f64>,
    #[serde(default)]
    pub disk_mb: Option<f64>,
    #[serde(default)]
    pub disk_max_mb: Option<f64>,
    #[serde(default)]
    pub uptime_seconds: Option<f64>,
    #[serde(default)]
    pub players: Option<f64>,
    #[serde(default)]
    pub address: Option<Address>,
    #[serde(default)]
    pub node: Option<String>,
}

// ── Métadonnées ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct Meta {
    /// Clé de configuration `PROVIDER=` — documentée même si le code ne la
    /// relit pas (elle sert au registry et aux messages d'erreur).
    #[allow(dead_code)]
    pub name: &'static str,
    pub display_name: &'static str,
    /// Catégorie libre (`game`, `vps`…) — partie du contrat, documente les
    /// providers ; consommée par la bannière de la version JS uniquement.
    #[allow(dead_code)]
    pub kind: &'static str,
    /// Emoji d'affichage — partie du contrat (parité JS).
    #[allow(dead_code)]
    pub icon: &'static str,
    pub default_api_url: &'static str,
}

// ── Authentification ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Auth {
    /// `Authorization: Bearer <token>` (la plupart des providers).
    Bearer(String),
    /// `X-Auth-Token: <token>` (Scaleway).
    XAuthToken(String),
    /// HTTP Basic : `user:password` encodé en base64 (UpCloud).
    Basic(String),
    /// Signature maison OVHcloud (`$1$` + SHA-1) sur chaque requête.
    Ovh(OvhKeys),
    /// En-tête arbitraire PRÉ-FORMATÉ `"Nom: valeur"` (provider générique).
    Header(String),
    /// Aucune authentification (provider générique sans clé).
    None,
}

#[derive(Debug, Clone)]
pub struct OvhKeys {
    pub app_key: String,
    pub app_secret: String,
    pub consumer_key: String,
}

// ── Plomberie HTTP commune ─────────────────────────────────────────────────

/// Timeout de la phase de CONNEXION TCP uniquement (réparti par adresse par
/// hyper-util ; la résolution DNS et la poignée de main TLS restent couvertes
/// par le timeout global de la requête). Sans lui, un réseau qui droppe les
/// paquets (firewall) faisait attendre le timeout GLOBAL complet — 30 s à
/// chaque scan du watchdog — avant que l'erreur soit classée. 10 s suffisent
/// largement pour tout hébergeur joignable.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout des GET d'état, surchargeable par `PROVIDER_GET_TIMEOUT_MS`
/// (millisecondes). Absente ou invalide → défaut du provider. Figé à la
/// construction du client, comme le reste de la configuration.
pub(crate) fn get_timeout(default: Duration) -> Duration {
    parse_get_timeout_override(std::env::var("PROVIDER_GET_TIMEOUT_MS").ok().as_deref(), default)
}

/// `None`/valeur invalide → défaut du provider ; sinon millisecondes > 0.
fn parse_get_timeout_override(value: Option<&str>, default: Duration) -> Duration {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(default)
}

/// Client HTTP configuré pour UN provider (auth, timeouts, base URL).
/// Équivalent des `axios.create(...)` du JS ; le pool de connexions
/// keep-alive est natif dans reqwest.
#[derive(Debug, Clone)]
pub struct Rest {
    client: reqwest::Client,
    base_url: String,
    auth: Auth,
    /// Timeout des lectures (état, métriques).
    get_timeout: Duration,
    /// Timeout des POST power (un stop YorkHost peut dépasser 15 s).
    power_timeout: Duration,
}

impl Rest {
    pub fn new(base_url: String, auth: Auth, get_timeout: Duration, power_timeout: Duration) -> Self {
        // `Accept: application/json` par défaut : tous les providers JS
        // l'envoyaient. reqwest pose déjà `Content-Type` avec `.json()`.
        let client = reqwest::Client::builder()
            .user_agent("apabot/2.0 (Rust; reqwest)")
            .connect_timeout(CONNECT_TIMEOUT)
            .default_headers(
                [("Accept", "application/json")]
                    .into_iter()
                    .map(|(k, v)| (k.parse().unwrap(), v.parse().unwrap()))
                    .collect(),
            )
            .build()
            .expect("construction du client reqwest");
        Self { client, base_url, auth, get_timeout, power_timeout }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    /// Applique l'authentification (Bearer / X-Auth-Token / Basic).
    /// OVH est traité séparément dans [`Rest::send`] (signature par requête).
    fn apply_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            Auth::Bearer(token) => builder.bearer_auth(token),
            Auth::XAuthToken(token) => builder.header("X-Auth-Token", token),
            Auth::Basic(user_pass) => {
                builder.header("Authorization", format!("Basic {}", BASE64.encode(user_pass)))
            }
            Auth::Header(preformatted) => match preformatted.split_once(':') {
                Some((name, value)) => builder.header(name.trim(), value.trim()),
                None => builder,
            },
            Auth::None => builder,
            Auth::Ovh(_) => builder,
        }
    }

    /// Envoie une requête et retourne le corps JSON (ou `Value::Null` si le
    /// corps est vide). Convertit les erreurs réseau en [`ProviderError`]
    /// classifiées (timeout / réseau injoignable / connexion interrompue).
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Value, ProviderError> {
        let mut builder = self.client.request(method.clone(), self.url(path));
        builder = self.apply_auth(builder);
        if let Some(b) = body {
            builder = builder.json(b);
        }
        // Signature OVHcloud : "$1$" + SHA1(AS + "+" + CK + "+" + METHOD +
        // "+" + QUERY + "+" + BODY + "+" + TSTAMP). Nos endpoints OVH n'ont
        // pas de query string : le chemin signé = chemin de l'URL.
        if let Auth::Ovh(keys) = &self.auth {
            let body_str = body
                .map(|b| serde_json::to_string(b).unwrap_or_default())
                .unwrap_or_default();
            let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            let to_sign = format!(
                "{}+{}+{}+{}+{}+{}",
                keys.app_secret,
                keys.consumer_key,
                method.as_str(),
                path,
                body_str,
                timestamp
            );
            let signature = format!("$1${:x}", Sha1::digest(to_sign.as_bytes()));
            builder = builder
                .header("X-Ovh-Application", &keys.app_key)
                .header("X-Ovh-Consumer", &keys.consumer_key)
                .header("X-Ovh-Timestamp", timestamp.to_string())
                .header("X-Ovh-Signature", signature);
        }

        let response = builder
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| ProviderError::from_reqwest(&e))?;
        let status = response.status();
        if !status.is_success() {
            // Le champ `code` du corps JSON est conservé pour les messages
            // d'erreur (certains providers renvoient un code métier).
            let code = response
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(String::from));
            return Err(ProviderError::http(
                status.as_u16(),
                code,
                format!("HTTP {status}"),
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| ProviderError::from_reqwest(&e))?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes)
            .map_err(|e| ProviderError::other(format!("réponse JSON invalide : {e}")))
    }

    /// GET avec le timeout « lecture ».
    pub async fn get_json(&self, path: &str) -> Result<Value, ProviderError> {
        self.send(Method::GET, path, None, self.get_timeout).await
    }

    /// Accesseur de test : base URL réelle du client (zone Scaleway incluse).
    #[cfg(test)]
    pub fn base_url_for_test(&self) -> &str {
        &self.base_url
    }
    /// POST avec corps JSON et le timeout « power ».
    pub async fn post_json(&self, path: &str, body: &Value) -> Result<Value, ProviderError> {
        self.send(Method::POST, path, Some(body), self.power_timeout).await
    }

    /// POST sans corps avec le timeout « power ».
    pub async fn post_empty(&self, path: &str) -> Result<Value, ProviderError> {
        self.send(Method::POST, path, None, self.power_timeout).await
    }
}

// ── Contrat Provider ───────────────────────────────────────────────────────

pub trait Provider: Send + Sync {
    fn meta(&self) -> &'static Meta;

    /// Accès à la plomberie HTTP commune — surtout utile aux tests (URL de
    /// base, timeouts) ; le code de production passe par les méthodes
    /// `fetch_raw`/`send_power_raw`.
    #[allow(dead_code)]
    fn rest(&self) -> &Rest;

    /// Lit l'état brut du service (`target` = identifiant chez l'hébergeur).
    async fn fetch_raw(&self, target: &str) -> Result<Value, ProviderError>;

    /// Envoie UNE action power (`start` | `stop` | `restart`).
    /// Contrat : jamais de retry interne — l'appelant gère l'incertitude.
    async fn send_power_raw(&self, target: &str, action: &str) -> Result<(), ProviderError>;

    /// Traduit la réponse brute vers l'état canonique [`Status`].
    fn normalize(&self, raw: &Value) -> Status;
}

// ── Registry ───────────────────────────────────────────────────────────────

/// Dispatch STATIQUE des providers : un enum plutôt qu'un `Box<dyn Provider>`.
///
/// Pourquoi ? Deux raisons, pédagogiques et techniques :
/// 1. Un trait avec des méthodes `async` n'est pas utilisable en trait object
///    (`dyn Provider`) sans la crate `async_trait` — l'enum évite cette
///    dépendance ET l'indirection de vtable : chaque appel est résolu à la
///    compilation (meilleur pour le CPU, dans l'esprit « perf » du portage).
/// 2. L'enum documente EXHAUSTIVEMENT les providers : en ajouter un est une
///    erreur de compilation partout où le match est exhaustif — le compilateur
///    vous dit exactement quoi mettre à jour (meilleur que le registry JS).
#[derive(Debug, Clone)]
pub enum AnyProvider {
    Yorkhost(yorkhost::Yorkhost),
    Hetzner(hetzner::Hetzner),
    Nitrado(nitrado::Nitrado),
    Ovh(ovh::Ovh),
    Scaleway(scaleway::Scaleway),
    Digitalocean(digitalocean::Digitalocean),
    Vultr(vultr::Vultr),
    Upcloud(upcloud::Upcloud),
    Generic(generic::Generic),
}

impl AnyProvider {
    /// Construit le provider nommé par `PROVIDER=`. `api_url` surcharge l'URL
    /// de base (équivalent `PROVIDER_API_URL` du JS).
    pub fn build(name: &str, api_key: &str, api_url: Option<&str>) -> Result<Self, crate::errors::ExitError> {
        let base = api_url.filter(|u| !u.trim().is_empty());
        Ok(match name {
            "yorkhost" => AnyProvider::Yorkhost(yorkhost::Yorkhost::new(api_key, base)),
            "hetzner" => AnyProvider::Hetzner(hetzner::Hetzner::new(api_key, base)),
            "nitrado" => AnyProvider::Nitrado(nitrado::Nitrado::new(api_key, base)),
            "ovh" => AnyProvider::Ovh(ovh::Ovh::new(api_key, base)?),
            "scaleway" => AnyProvider::Scaleway(scaleway::Scaleway::new(api_key, base)),
            "digitalocean" => AnyProvider::Digitalocean(digitalocean::Digitalocean::new(api_key, base)),
            "vultr" => AnyProvider::Vultr(vultr::Vultr::new(api_key, base)),
            "upcloud" => AnyProvider::Upcloud(upcloud::Upcloud::new(api_key, base)),
            "generic" => AnyProvider::Generic(generic::Generic::new(api_key, base)?),
            _ => {
                return Err(crate::errors::ExitError::config(format!(
                    "Unknown provider \"{name}\". Known: \"yorkhost\", \"hetzner\", \"nitrado\", \"ovh\", \"scaleway\", \"digitalocean\", \"vultr\", \"upcloud\", \"generic\""
                )))
            }
        })
    }
}

impl Provider for AnyProvider {
    fn meta(&self) -> &'static Meta {
        match self {
            AnyProvider::Yorkhost(p) => p.meta(),
            AnyProvider::Hetzner(p) => p.meta(),
            AnyProvider::Nitrado(p) => p.meta(),
            AnyProvider::Ovh(p) => p.meta(),
            AnyProvider::Scaleway(p) => p.meta(),
            AnyProvider::Digitalocean(p) => p.meta(),
            AnyProvider::Vultr(p) => p.meta(),
            AnyProvider::Upcloud(p) => p.meta(),
            AnyProvider::Generic(p) => p.meta(),
        }
    }

    fn rest(&self) -> &Rest {
        match self {
            AnyProvider::Yorkhost(p) => p.rest(),
            AnyProvider::Hetzner(p) => p.rest(),
            AnyProvider::Nitrado(p) => p.rest(),
            AnyProvider::Ovh(p) => p.rest(),
            AnyProvider::Scaleway(p) => p.rest(),
            AnyProvider::Digitalocean(p) => p.rest(),
            AnyProvider::Vultr(p) => p.rest(),
            AnyProvider::Upcloud(p) => p.rest(),
            AnyProvider::Generic(p) => p.rest(),
        }
    }

    async fn fetch_raw(&self, target: &str) -> Result<Value, ProviderError> {
        match self {
            AnyProvider::Yorkhost(p) => p.fetch_raw(target).await,
            AnyProvider::Hetzner(p) => p.fetch_raw(target).await,
            AnyProvider::Nitrado(p) => p.fetch_raw(target).await,
            AnyProvider::Ovh(p) => p.fetch_raw(target).await,
            AnyProvider::Scaleway(p) => p.fetch_raw(target).await,
            AnyProvider::Digitalocean(p) => p.fetch_raw(target).await,
            AnyProvider::Vultr(p) => p.fetch_raw(target).await,
            AnyProvider::Upcloud(p) => p.fetch_raw(target).await,
            AnyProvider::Generic(p) => p.fetch_raw(target).await,
        }
    }

    async fn send_power_raw(&self, target: &str, action: &str) -> Result<(), ProviderError> {
        match self {
            AnyProvider::Yorkhost(p) => p.send_power_raw(target, action).await,
            AnyProvider::Hetzner(p) => p.send_power_raw(target, action).await,
            AnyProvider::Nitrado(p) => p.send_power_raw(target, action).await,
            AnyProvider::Ovh(p) => p.send_power_raw(target, action).await,
            AnyProvider::Scaleway(p) => p.send_power_raw(target, action).await,
            AnyProvider::Digitalocean(p) => p.send_power_raw(target, action).await,
            AnyProvider::Vultr(p) => p.send_power_raw(target, action).await,
            AnyProvider::Upcloud(p) => p.send_power_raw(target, action).await,
            AnyProvider::Generic(p) => p.send_power_raw(target, action).await,
        }
    }

    fn normalize(&self, raw: &Value) -> Status {
        match self {
            AnyProvider::Yorkhost(p) => p.normalize(raw),
            AnyProvider::Hetzner(p) => p.normalize(raw),
            AnyProvider::Nitrado(p) => p.normalize(raw),
            AnyProvider::Ovh(p) => p.normalize(raw),
            AnyProvider::Scaleway(p) => p.normalize(raw),
            AnyProvider::Digitalocean(p) => p.normalize(raw),
            AnyProvider::Vultr(p) => p.normalize(raw),
            AnyProvider::Upcloud(p) => p.normalize(raw),
            AnyProvider::Generic(p) => p.normalize(raw),
        }
    }
}

// ── Aides de normalisation partagées ───────────────────────────────────────

/// Accès tolérant à un chemin de clés dans un JSON (`a.b.c`).
pub(crate) fn get<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

/// Nombre flottant à un chemin, en tolérant une chaîne numérique ("2048").
pub(crate) fn num(value: &Value, path: &[&str]) -> Option<f64> {
    match get(value, path)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Chaîne à un chemin (ou nombre converti en chaîne), `None` si absente/vide.
pub(crate) fn str_at(value: &Value, path: &[&str]) -> Option<String> {
    match get(value, path)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Dernier point d'une série `[[valeur, timestamp], …]` (stats Nitrado).
pub(crate) fn last_point(value: &Value, path: &[&str]) -> Option<f64> {
    let series = get(value, path)?.as_array()?;
    let last = series.last()?.as_array()?;
    num_at(last.first()?)
}

fn num_at(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Application d'une table d'états (`"started" → "running"`…) avec repli.
pub(crate) fn map_state(raw_state: &str, table: &[(&str, &str)], fallback: &str) -> String {
    table
        .iter()
        .find(|(from, _)| *from == raw_state)
        .map(|(_, to)| (*to).to_string())
        .unwrap_or_else(|| fallback.to_string())
}

/// Normalisation d'une adresse vers `{ip, port}` : accepte une chaîne
/// `"ip:port"` ou un objet `{ip|host, port}`.
pub(crate) fn normalize_address(value: &Value) -> Option<Address> {
    match value {
        Value::String(s) => {
            // JS : lastIndexOf(":") <= 0 → adresse sans port (pas de `:` du
            // tout, ou `:` en tête). Sinon on coupe sur le DERNIER `:`
            // (les IPv6 contiennent des `:`).
            match s.rfind(':') {
                Some(idx) if idx > 0 => {
                    let ip = s[..idx].to_string();
                    let port = s[idx + 1..].trim().parse::<u32>().ok();
                    Some(Address { ip, port })
                }
                _ => Some(Address { ip: s.clone(), port: None }),
            }
        }
        Value::Object(o) => {
            let ip = o
                .get("ip")
                .or_else(|| o.get("host"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let port = o.get("port").and_then(|v| match v {
                Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
                Value::String(s) => s.trim().parse::<u32>().ok(),
                _ => None,
            });
            match (ip, port) {
                (Some(ip), port) => Some(Address { ip, port }),
                (None, _) => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_timeout_override_parses_or_falls_back() {
        let default = Duration::from_secs(15);
        assert_eq!(parse_get_timeout_override(None, default), default);
        assert_eq!(parse_get_timeout_override(Some("nope"), default), default);
        assert_eq!(parse_get_timeout_override(Some("0"), default), default);
        assert_eq!(
            parse_get_timeout_override(Some(" 45000 "), default),
            Duration::from_secs(45)
        );
    }
}
