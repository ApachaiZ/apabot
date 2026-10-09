//! Gestion des membres de la guilde (portage de `lib/members.js`).
//!
//! Discord rate-limite sévèrement `REQUEST_GUILD_MEMBERS` (opcode 8 gateway) :
//! « request with opcode 8 was rate limited » — environ 1 chargement complet
//! par guilde toutes les 10 minutes. Politique centralisée ici :
//! - le menu `/users` est servi depuis le CACHE en mémoire, ou à défaut
//!   depuis la LISTE PERSISTÉE (`members.roster.json`, chiffrée) : les
//!   données d'un chargement précédent survivent aux redémarrages ;
//! - un rechargement complet n'a lieu que si le cache est vide ET que le
//!   dernier chargement réussi date d'avant le cooldown — jeton de date
//!   persisté (`members.state.json`), qui survit lui aussi aux restarts ;
//! - l'autocomplétion utilise la RECHERCHE REST de Discord (toujours à
//!   jour, aucun quota gateway) avec repli sur le roster persisté.

use poise::serenity_prelude as serenity;
use serde::{Deserialize, Serialize};
use serenity::GuildId;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::commands::{Context, Error};
use crate::crypto;
use crate::logger;
use crate::paths;

const COOLDOWN: Duration = Duration::from_secs(10 * 60);
/// Une liste persistée plus vieille que ça n'est plus proposée (membres
/// partis/rejoints trop incertains).
const ROSTER_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as f64
}

#[derive(Serialize, Deserialize)]
struct Roster {
    #[serde(rename = "savedAt")]
    saved_at: f64,
    members: Vec<RosterMember>,
}

#[derive(Serialize, Deserialize)]
struct RosterMember {
    id: String,
    username: String,
}

/// Lecture directe à chaque appel (rare) : l'état disque est la seule
/// source de vérité.
fn read_state() -> serde_json::Value {
    std::fs::read_to_string(paths::members_state_file())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn last_fetch_at() -> f64 {
    read_state().get("fetchedAt").and_then(|v| v.as_f64()).unwrap_or(0.0)
}

/// Écrit la liste persistée (humains uniquement, triés par pseudo),
/// chiffrée — la liste des membres ne doit pas être lisible en clair.
fn persist_roster(token: &str, mut members: Vec<(String, String)>) {
    members.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    let roster = Roster {
        saved_at: now_ms(),
        members: members
            .into_iter()
            .map(|(id, username)| RosterMember { id, username })
            .collect(),
    };
    if let Err(e) = crypto::write_json(
        token,
        &paths::members_roster_file(),
        &serde_json::to_value(roster).unwrap_or_default(),
    ) {
        logger::warn(format!(
            "members: impossible d'écrire {} : {e}",
            paths::members_roster_file().display()
        ));
    }
}

/// Liste persistée exploitable, ou `[]` si absente ou trop vieille.
/// Lecture tolérante : chiffré ou clair hérité.
fn read_persisted_roster(token: &str) -> Vec<(String, String)> {
    let Some(data) = crypto::read_json(token, &paths::members_roster_file()) else {
        return Vec::new();
    };
    let saved_at = data.get("savedAt").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let members = data
        .get("members")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    if members.is_empty() || now_ms() - saved_at > ROSTER_MAX_AGE.as_millis() as f64 {
        return Vec::new();
    }
    members
        .into_iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_string();
            let username = m.get("username")?.as_str()?.to_string();
            Some((id, username))
        })
        .collect()
}

/// Remplit le cache de membres de la guilde en respectant le cooldown
/// Discord. Retourne `true` si un chargement complet a réussi. Quand le
/// cache est déjà chaud, la liste persistée est rafraîchie gratuitement.
pub async fn refresh_members(ctx: &serenity::Context, guild_id: GuildId, token: &str) -> bool {
    // 1. Cache chaud → liste persistée rafraîchie gratuitement.
    if let Some(guild) = ctx.cache.guild(guild_id) {
        if !guild.members.is_empty() {
            let list: Vec<(String, String)> = guild
                .members
                .values()
                .filter(|m| !m.user.bot)
                .map(|m| (m.user.id.to_string(), m.user.name.clone()))
                .collect();
            persist_roster(token, list);
            return false;
        }
    }
    // 2. Cooldown opcode 8 respecté ?
    if now_ms() - last_fetch_at() < COOLDOWN.as_millis() as f64 {
        return false;
    }
    // 3. Chargement complet via la gateway (RequestGuildMembers) —
    //    `GuildId::members` est une async fn qui renvoie le Vec directement.
    let fetched: Vec<serenity::Member> = match guild_id.members(&ctx.http, None, None).await {
        Ok(members) => members,
        Err(e) => {
            logger::warn(format!(
                "members: chargement complet impossible (rate-limit opcode 8 ?) : {e}"
            ));
            return false;
        }
    };
    let list: Vec<(String, String)> = fetched
        .iter()
        .filter(|m| !m.user.bot)
        .map(|m| (m.user.id.to_string(), m.user.name.clone()))
        .collect();
    persist_roster(token, list);
    if write_state_file() {
        logger::info(format!("members: {} membres en cache.", fetched.len()));
    }
    true
}

fn write_state_file() -> bool {
    let state = serde_json::json!({ "fetchedAt": now_ms() });
    crate::fsutil::atomic_write(
        &paths::members_state_file(),
        state.to_string().as_bytes(),
    )
    .is_ok()
}

/// Entrée de roster générique : membre du cache ou pseudo-membre de la
/// liste persistée (seuls id/username sont exploités pour les suggestions).
#[derive(Clone)]
struct RosterEntry {
    id: String,
    username: String,
    bot: bool,
}

/// Suggestions d'autocomplétion pour l'option « membre » de /users.
/// Source principale : la recherche REST de Discord (données à jour, aucun
/// quota gateway) ; repli sur le cache/roster persisté si elle échoue.
pub async fn member_choices(
    ctx: &Context<'_>,
    partial: &str,
    subcommand: &str,
) -> Result<Vec<serenity::AutocompleteChoice>, Error> {
    let data = ctx.data();
    let users = data.state.get_users();
    let valid = |entry: &RosterEntry| {
        !entry.bot
            && !data.state.is_owner(&entry.id)
            && match subcommand {
                "add" => !users.contains(&entry.id),
                _ => users.contains(&entry.id),
            }
    };
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(Vec::new());
    };
    let typed = partial.trim().to_lowercase();

    // Source des suggestions : recherche REST (données à jour) si l'utilisateur
    // a commencé à taper ; repli sur le cache/roster si elle échoue. Champ
    // vide : liste complète depuis le cache/le persisté.
    let mut entries;
    let mut from_roster = false;
    if !typed.is_empty() {
        match guild_id.search_members(ctx.http(), &typed, Some(25)).await {
            Ok(members) => {
                entries = members
                    .into_iter()
                    .map(|m| RosterEntry {
                        id: m.user.id.to_string(),
                        username: m.user.name.clone(),
                        bot: m.user.bot,
                    })
                    .collect();
            }
            // Recherche REST indisponible : repli sur cache/roster.
            Err(_) => {
                entries = roster_entries(ctx, guild_id).await;
                from_roster = true;
            }
        }
    } else {
        entries = roster_entries(ctx, guild_id).await;
        from_roster = true;
    }

    // La recherche REST matche déjà la frappe (pseudo ou surnom) ; le
    // filtre local n'est nécessaire que pour le repli roster.
    if from_roster {
        entries.retain(|e| e.username.to_lowercase().contains(&typed));
    }
    entries.retain(valid);
    entries.sort_by(|a, b| a.username.to_lowercase().cmp(&b.username.to_lowercase()));
    entries.truncate(25);
    Ok(entries
        .into_iter()
        .map(|e| serenity::AutocompleteChoice::new(e.username, e.id))
        .collect())
}

/// Cache en mémoire (tenu à jour par la gateway) sinon liste persistée.
async fn roster_entries(ctx: &Context<'_>, guild_id: GuildId) -> Vec<RosterEntry> {
    let data = ctx.data();
    // Rafraîchissement éventuel (respecte le cooldown opcode 8).
    refresh_members(ctx.serenity_context(), guild_id, &data.cfg.token).await;
    if let Some(guild) = ctx.serenity_context().cache.guild(guild_id) {
        if !guild.members.is_empty() {
            return guild
                .members
                .values()
                .map(|m| RosterEntry {
                    id: m.user.id.to_string(),
                    username: m.user.name.clone(),
                    bot: m.user.bot,
                })
                .collect();
        }
    }
    // Pseudo-membres à partir de la liste persistée.
    read_persisted_roster(&data.cfg.token)
        .into_iter()
        .map(|(id, username)| RosterEntry { id, username, bot: false })
        .collect()
}
