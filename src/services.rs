//! Parsing et résolution des services ciblés.
//!
//! Un « service » est un couple `(alias, id)` : l'**alias** est le nom
//! court utilisé par l'utilisateur (`/status main`), l'**id** est
//! l'identifiant chez l'hébergeur. Deux modes de configuration :
//!
//! - mono-cible  : `PROVIDER_SERVICE_ID=1234` → alias `default` ;
//! - multi-cibles: `PROVIDER_SERVICES=main=1234,survival=5678`.
//!
//! Le premier alias déclaré est le **service par défaut** (celui visé quand
//! aucune option `service` ni session utilisateur n'est définie).

use crate::providers::Status;

/// Détecte une valeur « non définie » au sens du bot : absente, vide, ou
/// encore un placeholder non rempli (`YOUR_...`) — le bot refuse de démarrer
/// sur un `.env` d'exemple.
pub fn is_unset(value: Option<&str>) -> bool {
    match value {
        None => true,
        Some(s) => {
            let t = s.trim();
            t.is_empty() || t.starts_with("YOUR_")
        }
    }
}

/// Parse `alias1=id1,alias2=id2`. En l'absence de valeur exploitable,
/// retombe sur `fallback_id` avec l'alias `default`.
///
/// Amélioration par rapport au JS : un alias dupliqué est ignoré (première
/// occurrence gagnante) — l'objet JS écrasait silencieusement l'ancienne
/// valeur, ce qui masquait une faute de frappe dans la config.
pub fn parse_services(raw: Option<&str>, fallback_id: Option<&str>) -> Vec<(String, String)> {
    let mut map: Vec<(String, String)> = Vec::new();
    if let Some(raw) = raw.filter(|r| !is_unset(Some(r))) {
        for pair in raw.split(',') {
            let Some(eq) = pair.find('=') else { continue };
            let alias = pair[..eq].trim();
            let id = pair[eq + 1..].trim();
            if !alias.is_empty() && !is_unset(Some(id)) && !map.iter().any(|(a, _)| a == alias) {
                map.push((alias.to_string(), id.to_string()));
            }
        }
    }
    if map.is_empty() {
        if let Some(f) = fallback_id.filter(|f| !is_unset(Some(f))) {
            map.push(("default".to_string(), f.trim().to_string()));
        }
    }
    map
}

/// Résout un alias (éventuellement `None` → service par défaut) en couple
/// `(alias, id)`. Erreur explicite si l'alias est inconnu.
pub fn pick_service<'a>(
    services: &'a [(String, String)],
    alias: Option<&str>,
) -> Result<(&'a str, &'a str), String> {
    let wanted = match alias {
        Some(a) if !is_unset(Some(a)) => a.trim(),
        _ => services.first().map(|(a, _)| a.as_str()).unwrap_or(""),
    };
    match services.iter().find(|(a, _)| a == wanted) {
        Some((a, id)) => Ok((a, id)),
        None => {
            let known: Vec<String> = services.iter().map(|(a, _)| format!("\"{a}\"")).collect();
            Err(format!("Unknown service \"{wanted}\". Known: {}", known.join(", ")))
        }
    }
}

/// Nom d'affichage d'un service : le nom transmis par l'API du provider s'il
/// existe, sinon l'alias (qui sert de pseudonyme configurable).
pub fn display_name(status: &Status, alias: &str) -> String {
    match status.name.as_deref() {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => alias.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_unset_catches_placeholders() {
        assert!(is_unset(None));
        assert!(is_unset(Some("  ")));
        assert!(is_unset(Some("YOUR_TOKEN")));
        assert!(!is_unset(Some("abc")));
    }

    #[test]
    fn parse_multi_target() {
        let s = parse_services(Some("main=123, survival=567"), None);
        assert_eq!(s, vec![("main".to_string(), "123".to_string()), ("survival".to_string(), "567".to_string())]);
    }

    #[test]
    fn parse_falls_back_to_single_id() {
        let s = parse_services(Some(""), Some("42"));
        assert_eq!(s, vec![("default".to_string(), "42".to_string())]);
    }

    #[test]
    fn parse_ignores_broken_pairs() {
        let s = parse_services(Some("nope, main=1, =2, x=YOUR_ID"), None);
        assert_eq!(s, vec![("main".to_string(), "1".to_string())]);
    }

    #[test]
    fn pick_defaults_to_first_alias() {
        let s = parse_services(Some("a=1,b=2"), None);
        assert_eq!(pick_service(&s, None).unwrap(), ("a", "1"));
        assert_eq!(pick_service(&s, Some("b")).unwrap(), ("b", "2"));
        assert!(pick_service(&s, Some("zz")).is_err());
    }

    #[test]
    fn display_name_prefers_api_name() {
        use serde_json::json;
        let s: Status = serde_json::from_value(json!({"name": "Mon Serveur", "state": "running"})).unwrap();
        assert_eq!(display_name(&s, "alias"), "Mon Serveur");
        let s2: Status = serde_json::from_value(json!({"state": "running"})).unwrap();
        assert_eq!(display_name(&s2, "alias"), "alias");
    }
}
