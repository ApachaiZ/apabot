//! Classification des erreurs d'API provider + erreur de sortie spéciale.
//!
//! Le portage de `lib/errors.js` : la version JS classait les erreurs axios
//! (`err.code`, `err.response.status`) pour décider — entre autres — si une
//! requête power avait PU être transmise malgré l'erreur (timeout, 5xx
//! tardif…). Cette décision garantit que le bot n'envoie JAMAIS deux
//! requêtes power pour une même intention.
//!
//! En Rust, reqwest expose moins de codes symboliques qu'axios : la
//! classification réseau est donc approchée en inspectant la chaîne de
//! causes (`std::error::Error::source`) pour détecter une connexion
//! interrompue APRÈS l'envoi (équivalent `ECONNRESET` d'axios).

use std::fmt;
use std::error::Error as StdError;

/// Nature d'une erreur de requête vers un provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetKind {
    /// La requête n'a jamais reçu de réponse dans le délai (axios ECONNABORTED…).
    Timeout,
    /// La connexion n'a jamais pu être établie (DNS, refus…) : rien n'est parti.
    Unreachable,
    /// La connexion a été interrompue APRÈS l'envoi : la requête a pu partir.
    Reset,
    /// Une réponse HTTP est revenue (quel que soit son statut).
    Http,
    /// Erreur non classifiée (erreur métier du provider, décodage…).
    Other,
}

/// Erreur normalisée remontée par la couche API du bot.
#[derive(Debug, Clone)]
pub struct ProviderError {
    pub status: Option<u16>,
    /// Champ `code` du corps de réponse JSON (certains providers en renvoient).
    pub code: Option<String>,
    pub kind: NetKind,
    pub message: String,
    /// Marqueur « la requête power est partie » (posé par l'appelant le cas
    /// échéant — équivalent de `err._powerSent` côté JS).
    pub power_sent: bool,
}

impl ProviderError {
    pub fn http(status: u16, code: Option<String>, message: impl Into<String>) -> Self {
        Self {
            status: Some(status),
            code,
            kind: NetKind::Http,
            message: message.into(),
            power_sent: false,
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self {
            status: None,
            code: None,
            kind: NetKind::Other,
            message: message.into(),
            power_sent: false,
        }
    }

    /// Convertit une erreur reqwest de TRANSPORT (pas de réponse HTTP) en
    /// [`ProviderError`].
    ///
    /// Classification — la phase de connexion est testée AVANT le timeout :
    /// - phase de connexion (`is_connect` : DNS, TCP, poignée de main TLS —
    ///   y compris un timeout de connexion, pour lequel `is_timeout()` vaut
    ///   AUSSI true) → `Unreachable` : rien n'a pu partir ;
    /// - timeout du RESTE de la requête → `Timeout` : la requête a pu partir
    ///   (politique conservatrice, voir [`may_have_been_transmitted`]) ;
    /// - erreur io interrompue APRÈS l'envoi (RST, EOF, pipe cassé) → `Reset`.
    ///
    /// Le message porte un libellé classifié : `Display` de reqwest 0.12
    /// n'affiche JAMAIS la cause (« error sending request for url (…) »
    /// tout court), ce qui rendait les logs du watchdog indéchiffrables.
    pub fn from_reqwest(error: &reqwest::Error) -> Self {
        let connect_phase = error.is_connect();
        let timed_out = error.is_timeout();
        let kind = if connect_phase {
            NetKind::Unreachable
        } else if timed_out {
            NetKind::Timeout
        } else {
            // Parcours de la chaîne de causes : hyper/rustls remontent les
            // erreurs io réelles (connexion réinitialisée, EOF inattendu…).
            // `source()` vient du trait `std::error::Error`, d'où l'import
            // `StdError` en tête de fichier.
            let mut source = error.source();
            let mut kind = NetKind::Other;
            while let Some(cause) = source {
                if let Some(io) = cause.downcast_ref::<std::io::Error>() {
                    use std::io::ErrorKind;
                    if matches!(
                        io.kind(),
                        ErrorKind::ConnectionReset | ErrorKind::UnexpectedEof | ErrorKind::BrokenPipe
                    ) {
                        kind = NetKind::Reset;
                        break;
                    }
                }
                source = cause.source();
            }
            kind
        };
        let label = match kind {
            NetKind::Unreachable if timed_out => Some("timeout de connexion"),
            NetKind::Unreachable => Some("connexion impossible"),
            NetKind::Timeout => Some("timeout de réponse"),
            NetKind::Reset => Some("connexion interrompue"),
            // Non classifié : on conserve le détail reqwest brut, sans préfixe.
            NetKind::Other => None,
            NetKind::Http => unreachable!("from_reqwest ne produit jamais d'erreur HTTP"),
        };
        network_error(kind, label, error.to_string())
    }
}

/// Fabrique une [`ProviderError`] réseau. `label` préfixe le détail reqwest
/// dans le message (lisible dans les logs, où `Display` de reqwest 0.12
/// n'affiche jamais la cause) ; `None` = erreur non classifiée : le détail
/// brut est conservé tel quel.
fn network_error(kind: NetKind, label: Option<&'static str>, detail: impl Into<String>) -> ProviderError {
    let detail = detail.into();
    let message = match label {
        Some(label) => format!("{label} — {detail}"),
        None => detail,
    };
    ProviderError {
        status: None,
        code: None,
        kind,
        message,
        power_sent: false,
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ProviderError {}

/// L'action a-t-elle pu être transmise au provider malgré l'erreur ?
/// - 4xx : le provider a REJETÉ la requête avant exécution → non ;
/// - 5xx : la réponse est arrivée APRÈS un traitement possible → oui ;
/// - timeout / connexion interrompue : la requête a pu partir → oui ;
/// - réseau injoignable / erreur sans statut HTTP : rien n'a pu partir → non.
pub fn may_have_been_transmitted(error: &ProviderError) -> bool {
    match error.kind {
        NetKind::Timeout | NetKind::Reset => true,
        NetKind::Http => error.status.is_some_and(|s| s >= 500),
        _ => false,
    }
}

/// Erreur qui porte un CODE DE SORTIE process (EX_CONFIG = 78). Le
/// superviseur embarqué (`ops::supervise`) et systemd ne redémarrent PAS
/// sur ce code : une erreur de configuration ne se répare pas en boucle,
/// contrairement à un vrai crash (exit 1).
#[derive(Debug, thiserror::Error)]
#[error("{msg}")]
pub struct ExitError {
    pub msg: String,
    pub code: i32,
}

impl ExitError {
    /// Erreur de configuration (code 78, EX_CONFIG des sysexits).
    pub fn config(msg: impl Into<String>) -> Self {
        Self { msg: msg.into(), code: 78 }
    }
}

/// Contexte d'affichage d'une erreur : les requêtes power ne sont JAMAIS
/// retentées automatiquement, le message le rappelle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorContext {
    Power,
    Other,
}

/// Traduction d'une [`ProviderError`] en message LOCALISÉ et actionnable.
/// Portage direct de `errorText()` de `lib/interaction.js` : chaque cause
/// (401, 403, 404, 429, 5xx, timeout, réseau, reset) a son message dédié,
/// et le cas « l'action a PU être appliquée » est dit explicitement.
pub fn error_text(error: &ProviderError, context: ErrorContext, catalog: &crate::i18n::Catalog) -> String {
    use crate::i18n::fill;

    let status_part = error
        .status
        .map(|s| fill(catalog.errors.request_failed_status, &[("status", &s.to_string())]))
        .unwrap_or_default();
    let code_part = error
        .code
        .as_deref()
        .map(|c| fill(catalog.errors.request_failed_code, &[("code", c)]))
        .unwrap_or_default();
    let suffix = if context == ErrorContext::Power {
        catalog.errors.request_failed_power_suffix
    } else {
        ""
    };

    let (template, message) = match error.kind {
        NetKind::Http => match error.status {
            Some(401) => (catalog.errors.request_failed_auth, String::new()),
            Some(403) => (catalog.errors.request_failed_forbidden, String::new()),
            Some(404) => (catalog.errors.request_failed_not_found, String::new()),
            Some(429) => (catalog.errors.request_failed_rate_limit, String::new()),
            Some(s) if s >= 500 => (
                if error.power_sent {
                    catalog.errors.request_failed_server_sent
                } else {
                    catalog.errors.request_failed_server
                },
                String::new(),
            ),
            _ => (catalog.errors.request_failed, String::new()),
        },
        NetKind::Timeout => (
            if error.power_sent {
                catalog.errors.request_failed_timeout_sent
            } else {
                catalog.errors.request_failed_timeout
            },
            String::new(),
        ),
        NetKind::Unreachable => (catalog.errors.request_failed_network, String::new()),
        NetKind::Reset => (
            if error.power_sent {
                catalog.errors.request_failed_reset_sent
            } else {
                catalog.errors.request_failed_reset
            },
            String::new(),
        ),
        // Erreur non classifiée (erreur métier provider…) : on montre le
        // message réel plutôt que d'accuser la clé API.
        NetKind::Other => (catalog.errors.request_failed_generic, error.message.clone()),
    };

    fill(
        template,
        &[
            ("statusPart", status_part.as_str()),
            ("codePart", code_part.as_str()),
            ("suffix", suffix),
            ("message", message.as_str()),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_5xx_may_have_been_transmitted() {
        let e = ProviderError::http(500, None, "boom");
        assert!(may_have_been_transmitted(&e));
        let e = ProviderError::http(401, None, "denied");
        assert!(!may_have_been_transmitted(&e));
    }

    #[test]
    fn network_kinds_classify_transmission() {
        let timeout = ProviderError {
            status: None,
            code: None,
            kind: NetKind::Timeout,
            message: "t".into(),
            power_sent: false,
        };
        assert!(may_have_been_transmitted(&timeout));
        let unreachable = ProviderError {
            status: None,
            code: None,
            kind: NetKind::Unreachable,
            message: "u".into(),
            power_sent: false,
        };
        assert!(!may_have_been_transmitted(&unreachable));
    }

    #[test]
    fn exit_error_has_config_code() {
        let e = ExitError::config("missing");
        assert_eq!(e.code, 78);
        assert!(e.to_string().contains("missing"));
    }

    #[test]
    fn network_error_message_carries_classified_label() {
        let e = network_error(
            NetKind::Timeout,
            Some("timeout de réponse"),
            "error sending request for url (https://x)",
        );
        assert_eq!(
            e.to_string(),
            "timeout de réponse — error sending request for url (https://x)"
        );
        // Timeout pendant la phase de connexion : libellé dédié, et rien
        // n'a pu partir (aucune seconde requête power ne sera envisagée).
        let e = network_error(
            NetKind::Unreachable,
            Some("timeout de connexion"),
            "error sending request for url (https://x)",
        );
        assert_eq!(
            e.to_string(),
            "timeout de connexion — error sending request for url (https://x)"
        );
        assert!(!may_have_been_transmitted(&e));
        // Non classifié : détail brut conservé sans préfixe.
        let e = network_error(NetKind::Other, None, "détail brut");
        assert_eq!(e.to_string(), "détail brut");
    }
}
