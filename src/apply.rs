// Aplica runas, feitiços e sets de itens no cliente.
use crate::lcu::Lcu;
use crate::opgg::Build;
use anyhow::Result;
use serde_json::{json, Value};

/// Tudo que o app cria começa com isso; só apagamos/substituímos o que é nosso.
const PREFIX: &str = "OP2";
const FLASH: i64 = 4;

fn ids(v: &Value) -> Vec<i64> {
    v.as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default()
}

/// Páginas de runas e sets de itens deixados pelo OP.GG ou pelo Blitz.
fn from_other_app(name: &str) -> bool {
    let n = name.to_uppercase();
    n.starts_with("OP.GG") || n.starts_with("BLITZ")
}

fn title(b: &Build) -> String {
    format!("{PREFIX} {} {}", b.champ.name, b.label())
}

pub async fn apply_runes(lcu: &Lcu, b: &Build) -> Result<String> {
    let r = &b.data["runes"];
    let mut perks = ids(&r["primary_rune_ids"]);
    if perks.is_empty() {
        return Ok("sem dados de runa".into());
    }
    perks.extend(ids(&r["secondary_rune_ids"]));
    perks.extend(ids(&r["stat_mod_ids"]));

    let mut page = json!({
        "name": title(b),
        "primaryStyleId": r["primary_page_id"],
        "subStyleId": r["secondary_page_id"],
        "selectedPerkIds": perks,
        "current": true,
    });

    // Edita a página OP2 existente no lugar. Apagar e recriar deixava um instante
    // em que o cliente ativava outra página, e o jogo podia começar com ela.
    let pages = lcu.get("/lol-perks/v1/pages").await?;
    let ours: Vec<&Value> = pages
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["name"].as_str().is_some_and(|n| n.starts_with(PREFIX)) && p["isDeletable"].as_bool() == Some(true))
        .collect();

    if let Some(existing) = ours.first() {
        let id = existing["id"].clone();        page["id"] = id.clone();
        lcu.put(&format!("/lol-perks/v1/pages/{id}"), &page).await?;
        lcu.put("/lol-perks/v1/currentpage", &id).await?;
        for extra in &ours[1..] {
            let _ = lcu.delete(&format!("/lol-perks/v1/pages/{}", extra["id"])).await;
        }
    } else {
        let inv = lcu.get("/lol-perks/v1/inventory").await?;
        if inv["canAddCustomPage"].as_bool() == Some(false) {
            return Ok("sem espaço para nova página de runas (apague uma no cliente)".into());
        }
        lcu.post("/lol-perks/v1/pages", &page).await?;
    }

    // Páginas deixadas por outros apps (OP.GG, Blitz) atrapalham e podem ficar por cima da nossa.
    let mut removed = 0;
    for p in pages.as_array().into_iter().flatten() {
        if p["name"].as_str().is_some_and(from_other_app) && p["isDeletable"].as_bool() == Some(true) {
            if lcu.delete(&format!("/lol-perks/v1/pages/{}", p["id"])).await.is_ok() {
                removed += 1;
            }
        }
    }
    if removed > 0 {
        // garante que a nossa continua ativa depois de apagar as outras
        let pages = lcu.get("/lol-perks/v1/pages").await?;
        if let Some(id) = pages.as_array().into_iter().flatten().find(|p| p["name"] == page["name"]).map(|p| p["id"].clone()) {
            lcu.put("/lol-perks/v1/currentpage", &id).await?;
        }
        return Ok(format!("ok ({removed} página(s) do OP.GG/Blitz removida(s))"));
    }
    Ok("ok".into())
}

/// flash_key: "auto" (mantém o Flash onde ele já está), "D" ou "F"
pub async fn apply_spells(lcu: &Lcu, b: &Build, flash_key: &str) -> Result<String> {
    let spells = ids(&b.data["summoner_spells"]["ids"]);
    if spells.len() < 2 {
        return Ok("sem dados de feitiço".into());
    }
    let (mut s1, mut s2) = (spells[0], spells[1]);
    if s1 == FLASH || s2 == FLASH {
        let on_f = match flash_key {
            "D" => false,
            "F" => true,
            _ => {
                let s = lcu.get("/lol-champ-select/v1/session").await?;
                let cell = &s["localPlayerCellId"];
                s["myTeam"]
                    .as_array()
                    .and_then(|t| t.iter().find(|m| &m["cellId"] == cell))
                    .is_some_and(|me| me["spell2Id"].as_i64() == Some(FLASH))
            }
        };
        let other = if s1 == FLASH { s2 } else { s1 };
        (s1, s2) = if on_f { (other, FLASH) } else { (FLASH, other) };
    }
    lcu.patch("/lol-champ-select/v1/session/my-selection", &json!({ "spell1Id": s1, "spell2Id": s2 }))
        .await?;
    Ok("ok".into())
}

fn block(title: String, item_ids: &[i64]) -> Value {
    // agrupa repetidos (ex: 2 poções -> count 2), mantendo a ordem
    let mut counts: Vec<(String, i64)> = Vec::new();
    for id in item_ids {
        let s = id.to_string();
        match counts.iter_mut().find(|(k, _)| *k == s) {
            Some(e) => e.1 += 1,
            None => counts.push((s, 1)),
        }
    }
    json!({
        "type": title,
        "items": counts.iter().map(|(id, c)| json!({ "id": id, "count": c })).collect::<Vec<_>>(),
        "hideIfSummonerSpell": "",
        "showIfSummonerSpell": "",
    })
}

/// O app do OP.GG (e o Blitz) também gravam sets como arquivos em
/// `<LoL>\Config\Global\Recommended` e `<LoL>\Config\Champions\*\Recommended`.
/// Os "Global" aparecem para todos os campeões. Apaga os que são deles.
pub async fn remove_other_app_files(lcu: &Lcu) -> usize {
    let dir = lcu
        .get("/data-store/v1/install-dir")
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| r"C:\Riot Games\League of Legends".into());
    let config = std::path::Path::new(&dir).join("Config");
    let mut folders = vec![config.join("Global").join("Recommended")];
    if let Ok(champs) = std::fs::read_dir(config.join("Champions")) {
        folders.extend(champs.flatten().map(|c| c.path().join("Recommended")));
    }
    let mut removed = 0;
    for folder in folders {
        let Ok(files) = std::fs::read_dir(&folder) else { continue };
        for f in files.flatten() {
            let path = f.path();
            let name = f.file_name().to_string_lossy().to_lowercase();
            let by_name = name.contains("opgg") || name.contains("blitz");
            let by_title = || {
                std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(t.trim_start_matches('\u{feff}')).ok())
                    .is_some_and(|j| j["title"].as_str().is_some_and(from_other_app))
            };
            if name.ends_with(".json") && (by_name || by_title()) && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// Remove todos os sets de itens criados pelo OP2 (usado quando sai patch novo,
/// para não sobrar build velha de campeão que você não jogou desde então).
pub async fn remove_our_item_sets(lcu: &Lcu, summoner_id: i64) -> Result<usize> {
    let path = format!("/lol-item-sets/v1/item-sets/{summoner_id}/sets");
    let current = lcu.get(&path).await?;
    let all = current["itemSets"].as_array().cloned().unwrap_or_default();
    let keep: Vec<Value> =
        all.iter().filter(|s| !s["title"].as_str().is_some_and(|t| t.starts_with(PREFIX))).cloned().collect();
    let removed = all.len() - keep.len();
    if removed > 0 {
        lcu.put(
            &path,
            &json!({ "accountId": current["accountId"], "itemSets": keep, "timestamp": chrono::Utc::now().timestamp_millis() }),
        )
        .await?;
    }
    Ok(removed)
}

pub async fn apply_items(lcu: &Lcu, b: &Build, summoner_id: i64) -> Result<String> {
    let d = &b.data;
    let join = |v: &Value, sep: &str, n: usize| -> String {
        v.as_array()
            .map(|a| a.iter().filter_map(Value::as_str).take(n).collect::<Vec<_>>().join(sep))
            .unwrap_or_default()
    };
    let skill_order = join(&d["skill_masteries"]["ids"], " > ", 3);
    let first_skills = join(&d["skills"]["order"], " ", 6);

    let mut blocks = Vec::new();
    let starters = ids(&d["starter_items"]["ids"]);
    if !starters.is_empty() {
        let t = if skill_order.is_empty() { "Iniciais".into() } else { format!("Iniciais | Skills: {skill_order}") };
        blocks.push(block(t, &starters));
    }
    let boots = ids(&d["boots"]["ids"]);
    if !boots.is_empty() {
        let t = if first_skills.is_empty() { "Botas".into() } else { format!("Botas | Início: {first_skills}") };
        blocks.push(block(t, &boots));
    }
    let core = ids(&d["core_items"]["ids"]);
    if !core.is_empty() {
        let c = &d["core_items"];
        let wr = match (c["win"].as_f64(), c["play"].as_f64()) {
            (Some(w), Some(p)) if p > 0.0 => format!(" | Win {:.1}%", w / p * 100.0),
            _ => String::new(),
        };
        blocks.push(block(format!("Core{wr}"), &core));
    }
    for (key, name) in [("fourth_items", "4º item"), ("fifth_items", "5º item"), ("sixth_items", "6º item")] {
        let opts: Vec<i64> = d[key].as_array().into_iter().flatten().take(4).flat_map(|o| ids(&o["ids"])).collect();
        if !opts.is_empty() {
            blocks.push(block(format!("{name} (opções)"), &opts));
        }
    }
    if blocks.is_empty() {
        return Ok("sem dados de item".into());
    }

    let path = format!("/lol-item-sets/v1/item-sets/{summoner_id}/sets");
    let current = lcu.get(&path).await?;
    let champ_id = b.champ.id;
    let mut sets: Vec<Value> = current["itemSets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| {
            let ours = s["title"].as_str().is_some_and(|t| t.starts_with(PREFIX));
            let same_champ = s["associatedChampions"].as_array().is_some_and(|a| a.iter().any(|c| c.as_i64() == Some(champ_id)));
            !(ours && same_champ) && !s["title"].as_str().is_some_and(from_other_app)
        })
        .cloned()
        .collect();
    sets.insert(
        0,
        json!({
            "title": title(b),
            "type": "custom",
            "map": "any",
            "mode": "any",
            "sortrank": 0,
            "startedFrom": "blank",
            "associatedChampions": [champ_id],
            "associatedMaps": [],
            "preferredItemSlots": [],
            "uid": uuid::Uuid::new_v4().to_string(),
            "blocks": blocks,
        }),
    );
    lcu.put(
        &path,
        &json!({
            "accountId": current["accountId"],
            "itemSets": sets,
            "timestamp": chrono::Utc::now().timestamp_millis(),
        }),
    )
    .await?;
    Ok("ok".into())
}
