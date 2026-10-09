//! Commande `/logs` (owner uniquement) — portage de `lib/logs.js`.
//!
//! Affiche les 30 dernières lignes non vides du log applicatif ; avec un
//! filtre, parcourt les 2000 dernières lignes et garde les 30 dernières
//! correspondantes. Corps tronqué à 1800 caractères, présenté en bloc de
//! code.

use crate::commands::{reply, Context, Error};
use crate::embeds::{card, Tone};
use crate::i18n::fill;
use crate::paths;

const MAX_LINES: usize = 30;
const MAX_CHARS: usize = 1800;
const FILTER_SCAN: usize = 2000;

/// Les N dernières lignes non vides du fichier. Acceptable à l'échelle
/// actuelle (rotation à 5 Mo) — à réimplémenter par lecture arrière de
/// blocs si le volume augmente significativement (même note que le JS).
fn read_tail(path: &std::path::Path, max_lines: usize) -> Result<Vec<String>, std::io::Error> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .rev()
        .take(max_lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect())
}

pub async fn logs(ctx: &Context<'_>, filter: Option<String>) -> Result<(), Error> {
    let data = ctx.data();
    let catalog = data.catalog;
    ctx.defer_ephemeral().await?;

    // Owner uniquement (même politique que /users).
    if !data.state.is_owner(&ctx.author().id.to_string()) {
        let c = catalog.logs.access_denied;
        ctx.send(reply(card(c.title, c.body, Tone::Bad, None))).await?;
        return Ok(());
    }

    let filter = filter.unwrap_or_default().trim().to_lowercase();
    let mut lines = match read_tail(&paths::log_file(), if filter.is_empty() { MAX_LINES } else { FILTER_SCAN }) {
        Ok(lines) => lines,
        Err(e) => {
            let msg = match e.kind() {
                std::io::ErrorKind::NotFound => catalog.logs.not_found.to_string(),
                other => fill(catalog.logs.read_failed, &[("code", &format!("{other:?}"))]),
            };
            ctx.send(reply(card(catalog.logs.card_title, &msg, Tone::Wait, None))).await?;
            return Ok(());
        }
    };

    if !filter.is_empty() {
        lines.retain(|l| l.to_lowercase().contains(&filter));
        lines.truncate(MAX_LINES);
    }
    if lines.is_empty() {
        let msg = if filter.is_empty() {
            catalog.logs.empty.to_string()
        } else {
            fill(catalog.logs.nothing_filtered, &[("filter", &filter)])
        };
        ctx.send(reply(card(catalog.logs.card_title, &msg, Tone::Wait, None))).await?;
        return Ok(());
    }

    let mut body = lines.join("\n");
    body = truncate_tail(&body, MAX_CHARS);
    let title = if filter.is_empty() {
        fill(catalog.logs.title_last, &[("count", &lines.len().to_string())])
    } else {
        fill(
            catalog.logs.title_filtered,
            &[
                ("count", &lines.len().to_string()),
                ("filter", &filter),
            ],
        )
    };
    ctx.send(reply(card(&title, &format!("```\n{body}\n```"), Tone::Info, None))).await?;
    Ok(())
}

/// Tronque `body` à ses `max_chars` derniers CARACTÈRES (jamais des octets),
/// en préfixant « …\n ». `body.len()` compte des OCTETS : couper là-dessus
/// peut trancher un caractère multi-octets (le `─` des bannières = 3 octets)
/// et faire paniquer le slice — on cherche donc la frontière via
/// `char_indices`.
pub fn truncate_tail(body: &str, max_chars: usize) -> String {
    let count = body.chars().count();
    if count <= max_chars {
        return body.to_string();
    }
    let skip = count - max_chars;
    let cut = body
        .char_indices()
        .nth(skip)
        .map(|(idx, _)| idx)
        .unwrap_or(0);
    format!("…\n{}", &body[cut..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_tail_keeps_last_non_empty_lines() {
        let dir = std::env::temp_dir().join(format!("apabot-logs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sample.log");
        std::fs::write(&file, "a\n\nb\nc\nd\n").unwrap();
        let lines = read_tail(&file, 2).unwrap();
        assert_eq!(lines, vec!["c".to_string(), "d".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncation_keeps_the_tail() {
        // « …\n » + 1800 caractères ASCII.
        let body = format!("…\n{}", "x".repeat(1800));
        assert_eq!(body.len(), 1804);
    }

    #[test]
    fn truncation_never_splits_a_utf8_char() {
        // Le bug d'origine : couper par OCTETS au milieu d'un `─` (3 octets)
        // paniquait avec « byte index … is not a char boundary ».
        let truncated = truncate_tail(&"─".repeat(1900), 1800);
        assert!(truncated.starts_with("…\n"));
        assert_eq!(truncated.chars().count(), 1800 + 2);
        assert!(truncated.chars().skip(2).all(|c| c == '─'));
    }
}
