//! Assistant de première configuration (portage de `lib/setup.js`).
//!
//! Si des champs BLOQUANTS manquent au démarrage :
//! - `--non-interactive` OU pas de TTY → erreur immédiate listant les
//!   variables manquantes (exit 78 : le superviseur ne redémarre pas en
//!   boucle) ;
//! - sinon → prompts interactifs (dialoguer, avec masquage des secrets),
//!   réécriture de `.config.d/.env` en PRÉSERVANT commentaires et ordre
//!   existants, puis rechargement.
//!
//! La question de langue est posée en premier : sa réponse bascule
//! immédiatement les prompts suivants (portage de la « bascule à chaud »).

use std::collections::HashMap;
use std::io::{IsTerminal, Write};

use dialoguer::Input;
use dialoguer::Password;

use crate::errors::ExitError;
use crate::fsutil;
use crate::i18n::{fill, Catalog};
use crate::paths;
use crate::services::is_unset;

/// Un champ du questionnaire.
struct Field {
    key: &'static str,
    /// Clé du catalogue pour la question (résolue à CHAQUE prompt, pour
    /// suivre la bascule de langue).
    question: FieldQuestion,
    secret: bool,
    /// Jamais bloquant (BOT_LANGUAGE a un défaut côté i18n).
    optional: bool,
    /// Champ sauté si cette alternative est déjà présente.
    optional_if: Option<&'static str>,
    /// Valeur vide acceptée (cohérence vérifiée après la boucle complète).
    allow_empty: bool,
    choices: Option<&'static [&'static str]>,
}

#[derive(Clone, Copy)]
enum FieldQuestion {
    Language,
    DiscordToken,
    DiscordClientId,
    DiscordOwnerId,
    DiscordGuildId,
    ProviderApiKey,
    ProviderServiceId,
    ProviderServices,
}

impl FieldQuestion {
    /// Résout la question dans la langue ACTIVE.
    fn text(self, catalog: &Catalog) -> String {
        match self {
            FieldQuestion::Language => catalog.setup.language_label.to_string(),
            FieldQuestion::DiscordToken => catalog.setup.prompt.discord_token.to_string(),
            FieldQuestion::DiscordClientId => catalog.setup.prompt.discord_client_id.to_string(),
            FieldQuestion::DiscordOwnerId => catalog.setup.prompt.discord_owner_id.to_string(),
            FieldQuestion::DiscordGuildId => catalog.setup.prompt.discord_guild_id.to_string(),
            FieldQuestion::ProviderApiKey => catalog.setup.prompt.provider_api_key.to_string(),
            FieldQuestion::ProviderServiceId => catalog.setup.prompt.provider_service_id.to_string(),
            FieldQuestion::ProviderServices => catalog.setup.prompt.provider_services.to_string(),
        }
    }
}

/// Ordre canonique du questionnaire (identique au JS : la langue d'abord).
const FIELDS: [Field; 8] = [
    Field { key: "BOT_LANGUAGE", question: FieldQuestion::Language, secret: false, optional: true, optional_if: None, allow_empty: false, choices: Some(&["en", "fr"]) },
    Field { key: "DISCORD_TOKEN", question: FieldQuestion::DiscordToken, secret: true, optional: false, optional_if: None, allow_empty: false, choices: None },
    Field { key: "DISCORD_CLIENT_ID", question: FieldQuestion::DiscordClientId, secret: false, optional: false, optional_if: None, allow_empty: false, choices: None },
    Field { key: "DISCORD_OWNER_ID", question: FieldQuestion::DiscordOwnerId, secret: false, optional: false, optional_if: None, allow_empty: false, choices: None },
    Field { key: "DISCORD_GUILD_ID", question: FieldQuestion::DiscordGuildId, secret: false, optional: false, optional_if: None, allow_empty: false, choices: None },
    Field { key: "PROVIDER_API_KEY", question: FieldQuestion::ProviderApiKey, secret: true, optional: false, optional_if: None, allow_empty: false, choices: None },
    Field { key: "PROVIDER_SERVICE_ID", question: FieldQuestion::ProviderServiceId, secret: false, optional: false, optional_if: Some("PROVIDER_SERVICES"), allow_empty: true, choices: None },
    Field { key: "PROVIDER_SERVICES", question: FieldQuestion::ProviderServices, secret: false, optional: false, optional_if: Some("PROVIDER_SERVICE_ID"), allow_empty: true, choices: None },
];

/// Parse un fichier `.env` en map clé → valeur (commentaires ignorés).
fn parse_env(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(idx) = line.find('=') else { continue };
        let key = line[..idx].trim();
        if !key.is_empty() {
            map.insert(key.to_string(), line[idx + 1..].trim().to_string());
        }
    }
    map
}

fn read_env_at(env_path: &std::path::Path) -> HashMap<String, String> {
    std::fs::read_to_string(env_path)
        .map(|t| parse_env(&t))
        .unwrap_or_default()
}

/// Réécrit le fichier `.env` en préservant les commentaires et l'ordre
/// existants : clés mises à jour en place, nouvelles ajoutées à la fin,
/// une seule ligne vide finale. Mode 0600.
fn write_env(map: &HashMap<String, String>, env_path: &std::path::Path) {
    let existing_lines = std::fs::read_to_string(env_path)
        .map(|t| t.lines().map(String::from).collect::<Vec<_>>())
        .unwrap_or_default();

    // Valeurs restantes à ajouter, dans l'ordre canonique des FIELDS puis
    // les éventuelles clés supplémentaires (comme le Map ordonné du JS).
    let mut remaining: Vec<(String, String)> = Vec::new();
    for f in &FIELDS {
        if let Some(v) = map.get(f.key) {
            remaining.push((f.key.to_string(), v.clone()));
        }
    }
    for (k, v) in map.iter() {
        if !FIELDS.iter().any(|f| f.key == k) {
            remaining.push((k.clone(), v.clone()));
        }
    }

    let mut out: Vec<String> = Vec::new();
    for line in existing_lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push(line);
            continue;
        }
        let Some(idx) = line.find('=') else {
            out.push(line);
            continue;
        };
        let key = line[..idx].trim();
        if let Some(pos) = remaining.iter().position(|(k, _)| k == key) {
            let (_, v) = remaining.remove(pos);
            out.push(format!("{key}={v}"));
        } else {
            out.push(line);
        }
    }
    for (k, v) in remaining {
        out.push(format!("{k}={v}"));
    }
    while out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        out.pop();
    }
    out.push(String::new());

    let contents = out.join("\n");
    // Un échec d'écriture du .env est fatal : sans lui, le bot ne pourra
    // jamais démarrer correctement.
    if let Err(e) = fsutil::secure_write(env_path, contents.as_bytes()) {
        eprintln!("Impossible d'écrire {} : {e}", env_path.display());
        std::process::exit(78);
    }
}

/// Un champ est-il satisfait par le `.env` OU l'environnement ?
fn has(map: &HashMap<String, String>, key: &str) -> bool {
    !is_unset(map.get(key).map(String::as_str))
        || !is_unset(std::env::var(key).ok().as_deref())
}

/// Vérifie la cohérence des champs `allow_empty` : si la valeur est vide,
/// son alternative doit être présente.
fn check_alternative(
    f: &Field,
    map: &HashMap<String, String>,
    catalog: &Catalog,
) -> Result<(), ExitError> {
    if !f.allow_empty {
        return Ok(());
    }
    if !is_unset(map.get(f.key).map(String::as_str)) {
        return Ok(());
    }
    let alt = f.optional_if.unwrap_or_default();
    let alt_ok = !alt.is_empty() && has(map, alt);
    if !alt_ok {
        return Err(ExitError::config(fill(
            catalog.errors.setup_incomplete_check,
            &[("key", f.key), ("alt", alt)],
        )));
    }
    Ok(())
}

/// Point d'entrée : garantit un environnement complet avant le démarrage.
pub fn ensure_env(non_interactive: bool, catalog: &'static Catalog) -> Result<(), ExitError> {
    ensure_env_at(&paths::env_file(), non_interactive, catalog)
}

/// Variante de [`ensure_env`] qui lit/écrit un fichier `.env` ARBITRAIRE —
/// utilisée par `apabot install` (configuration de la cible) et
/// `apabot config` (le `.env` actif, portage ou installation).
pub fn ensure_env_at(
    env_path: &std::path::Path,
    non_interactive: bool,
    catalog: &'static Catalog,
) -> Result<(), ExitError> {
    // Charger CE fichier (et non celui du CWD) dans l'environnement : `has`
    // consulte les variables, et les valeurs existantes préremplissent.
    let _ = dotenvy::from_path(env_path);
    let mut existing = read_env_at(env_path);

    let missing: Vec<&Field> = FIELDS
        .iter()
        .filter(|f| !f.optional && !has(&existing, f.key) && !f.optional_if.map(|alt| has(&existing, alt)).unwrap_or(false))
        .collect();

    let unanswered: Vec<&Field> = FIELDS
        .iter()
        .filter(|f| !has(&existing, f.key) && !f.optional_if.map(|alt| has(&existing, alt)).unwrap_or(false))
        .collect();

    let tty = std::io::stdin().is_terminal();
    if non_interactive || !tty {
        if !missing.is_empty() {
            let missing_keys: Vec<&str> = missing.iter().map(|f| f.key).collect();
            let template = if non_interactive {
                catalog.errors.setup_incomplete_non_interactive
            } else {
                catalog.errors.setup_incomplete_no_tty
            };
            return Err(ExitError::config(fill(
                template,
                &[
                    ("missing", &missing_keys.join(", ")),
                    ("envFile", &env_path.display().to_string()),
                ],
            )));
        }
        return Ok(());
    }
    if unanswered.is_empty() {
        return Ok(());
    }

    // ── Prompts interactifs ──
    print!("{}", catalog.setup.intro);
    print!("{}", fill(catalog.setup.file_line, &[("envFile", &env_path.display().to_string())]));
    let mut lang = catalog; // suit la bascule de langue pendant le prompt

    for f in &unanswered {
        loop {
            let question = f.question.text(lang);
            let answer = if f.secret {
                // Masquage du secret (dialoguer masque la saisie).
                match Password::new()
                    .with_prompt(fill(lang.setup.prompt_line_hidden, &[("question", &question)]))
                    .interact()
                {
                    Ok(a) => a,
                    Err(e) => {
                        return Err(ExitError::config(format!("setup interrompu : {e}")));
                    }
                }
            } else {
                match Input::<String>::new()
                    .with_prompt(fill(lang.setup.prompt_line, &[("question", &question)]))
                    .interact_text()
                {
                    Ok(a) => a,
                    Err(e) => {
                        return Err(ExitError::config(format!("setup interrompu : {e}")));
                    }
                }
            };
            let answer = answer.trim();
            if let Some(choices) = f.choices {
                if !choices.contains(&answer.to_lowercase().as_str()) {
                    print!("{}", lang.setup.invalid);
                    continue;
                }
            }
            if !is_unset(Some(answer)) {
                existing.insert(f.key.to_string(), answer.to_string());
                // Bascule immédiate : les prompts suivants changent de langue.
                if f.key == "BOT_LANGUAGE" {
                    lang = crate::i18n::catalog(answer);
                }
                break;
            }
            if f.allow_empty {
                break;
            }
            print!("{}", lang.setup.invalid);
        }
    }

    // Cohérence des champs vidables (l'un des deux au moins doit exister).
    // `lang` est le catalogue ACTIF après la question de langue.
    for f in &unanswered {
        check_alternative(f, &existing, lang)?;
    }

    write_env(&existing, env_path);
    print!("{}", fill(lang.setup.complete, &[("envFile", &env_path.display().to_string())]));
    let _ = std::io::stdout().flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_lines_and_ignores_comments() {
        let map = parse_env("A=1\n# comment\nB = 2\nC\n=4\nD=5=6");
        assert_eq!(map.get("A").unwrap(), "1");
        assert_eq!(map.get("B").unwrap(), "2");
        assert_eq!(map.get("D").unwrap(), "5=6"); // la valeur contient '='
        assert!(!map.contains_key("# comment"));
    }
}
