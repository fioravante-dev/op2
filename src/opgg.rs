// Busca builds no servidor MCP público do OP.gg (https://mcp-api.op.gg/mcp).
use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

const MCP_URL: &str = "https://mcp-api.op.gg/mcp";

const BUILD_FIELDS: &[&str] = &[
    "data.runes.{primary_page_id,primary_rune_ids[],secondary_page_id,secondary_rune_ids[],stat_mod_ids[],pick_rate,win,play}",
    "data.summoner_spells.{ids[],pick_rate,win,play}",
    "data.starter_items.{ids[],pick_rate,win,play}",
    "data.boots.{ids[],pick_rate,win,play}",
    "data.core_items.{ids[],pick_rate,win,play}",
    "data.fourth_items[].{ids[],pick_rate}",
    "data.fifth_items[].{ids[],pick_rate}",
    "data.sixth_items[].{ids[],pick_rate}",
    "data.skills.{order[]}",
    "data.skill_masteries.{ids[]}",
    "data.summary.average_stats.{win_rate,tier}",
];

#[derive(Clone)]
pub struct Champ {
    pub id: i64,
    pub key: String,
    pub name: String,
}

pub struct Build {
    pub champ: Champ,
    pub position: String,
    pub game_mode: String,
    pub data: Value,
}

impl Build {
    pub fn label(&self) -> String {
        if self.game_mode == "aram" {
            return "ARAM".into();
        }
        match self.position.as_str() {
            "top" => "Top",
            "jungle" => "Jungle",
            "mid" => "Mid",
            "adc" => "ADC",
            "support" => "Sup",
            other => other,
        }
        .into()
    }
}

type ChampMap = Arc<HashMap<i64, Champ>>;

pub struct OpGg {
    http: reqwest::Client,
    champs: Mutex<Option<ChampMap>>,
    rpc_id: AtomicU64,
}

impl OpGg {
    pub fn new() -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().expect("http client");
        Self { http, champs: Mutex::new(None), rpc_id: AtomicU64::new(1) }
    }

    /// Esquece a lista de campeões (novo patch / campeão novo); recarrega na próxima consulta.
    pub async fn refresh_champions(&self) {
        *self.champs.lock().await = None;
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": self.rpc_id.fetch_add(1, Ordering::Relaxed),
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        });
        let text = self
            .http
            .post(MCP_URL)
            .header("Accept", "application/json, text/event-stream")
            .json(&body)
            .send()
            .await?
            .text()
            .await?;
        // O servidor pode responder como SSE ("data: {...}")
        let text = if text.trim_start().starts_with('{') {
            text
        } else {
            text.lines().filter_map(|l| l.strip_prefix("data:")).collect::<Vec<_>>().join("")
        };
        let j: Value = serde_json::from_str(&text)?;
        if let Some(e) = j.get("error") {
            return Err(anyhow!("OP.gg: {}", e["message"]));
        }
        let out = j["result"]["content"][0]["text"].as_str().unwrap_or("");
        if j["result"]["isError"].as_bool() == Some(true) {
            return Err(anyhow!("OP.gg: {out}"));
        }
        parse_compact(out)
    }

    pub async fn champions(&self) -> Result<ChampMap> {
        let mut guard = self.champs.lock().await;
        if let Some(m) = guard.as_ref() {
            return Ok(m.clone());
        }
        let r = self
            .call_tool("lol_list_champions", json!({ "desired_output_fields": ["data.champions[].{champion_id,key,name}"] }))
            .await?;
        let list = r["data"]["champions"].as_array().ok_or_else(|| anyhow!("lista de campeões vazia"))?;
        let map: ChampMap = Arc::new(
            list.iter()
                .filter_map(|c| {
                    Some(Champ {
                        id: c["champion_id"].as_i64()?,
                        key: c["key"].as_str()?.to_string(),
                        name: c["name"].as_str()?.to_string(),
                    })
                })
                .map(|c| (c.id, c))
                .collect(),
        );
        *guard = Some(map.clone());
        Ok(map)
    }

    async fn champion(&self, id: i64) -> Result<Champ> {
        if let Some(c) = self.champions().await?.get(&id) {
            return Ok(c.clone());
        }
        // campeão novo que ainda não estava na lista em cache
        self.refresh_champions().await;
        self.champions().await?.get(&id).cloned().ok_or_else(|| anyhow!("Campeão {id} não encontrado no OP.gg"))
    }

    pub async fn find_champion(&self, query: &str) -> Result<Champ> {
        let norm = |s: &str| s.to_lowercase().chars().filter(|c| c.is_ascii_alphabetic()).collect::<String>();
        let q = norm(query);
        self.champions()
            .await?
            .values()
            .find(|c| norm(&c.name) == q || norm(&c.key) == q)
            .cloned()
            .ok_or_else(|| anyhow!("Campeão não encontrado: {query}"))
    }

    async fn analysis(&self, champ: &Champ, mode: &str, pos: &str, fields: &[&str]) -> Result<Value> {
        let mut last = anyhow!("sem nome para o campeão");
        for name in opgg_names(champ) {
            let args = json!({
                "game_mode": mode, "champion": name, "position": pos, "desired_output_fields": fields,
            });
            match self.call_tool("lol_get_champion_analysis", args).await {
                Ok(r) => return Ok(r["data"].clone()),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Rota principal do campeão (quando o cliente não informa, ex: normal blind)
    async fn main_position(&self, champ: &Champ) -> Result<String> {
        // O OP.gg às vezes responde sem o "summary"; tenta de novo algumas vezes.
        for attempt in 0..3 {
            let d = self.analysis(champ, "ranked", "mid", &["data.summary.positions[].name"]).await?;
            if let Some(name) = d["summary"]["positions"][0]["name"].as_str() {
                return Ok(name.to_lowercase());
            }
            tokio::time::sleep(Duration::from_millis(300 * (attempt + 1))).await;
        }
        Err(anyhow!("OP.gg não informou a rota"))
    }

    pub async fn get_build(&self, champion_id: i64, mode: &str, position: Option<String>) -> Result<Build> {
        let champ = self.champion(champion_id).await?;
        let position = match position {
            Some(p) => p,
            None if mode == "aram" => "mid".into(),
            None => self.main_position(&champ).await.unwrap_or_else(|e| {
                crate::util::log(&format!("  Rota principal indisponível ({e}); usando mid"));
                "mid".into()
            }),
        };
        let mut data = Value::Null;
        for attempt in 0..3 {
            data = self.analysis(&champ, mode, &position, BUILD_FIELDS).await?;
            if !data["runes"].is_null() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(300 * (attempt + 1))).await;
        }
        Ok(Build { champ, position, game_mode: mode.into(), data })
    }
}

/// "TwistedFate" -> "TWISTED_FATE"; também tenta pelo nome ("K'Sante" -> "KSANTE")
fn opgg_names(c: &Champ) -> Vec<String> {
    let chars: Vec<char> = c.key.chars().collect();
    let mut from_key = String::new();
    for (i, &ch) in chars.iter().enumerate() {
        if i > 0 && ch.is_uppercase() && chars[i - 1].is_lowercase() {
            from_key.push('_');
        }
        from_key.push(ch.to_ascii_uppercase());
    }
    let from_name = c
        .name
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic() || *ch == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_")
        .to_uppercase();
    if from_key == from_name { vec![from_key] } else { vec![from_key, from_name] }
}

/// O OP.gg responde num formato compacto:
///   class Data: core_items,runes
///   class Runes: primary_page_id,primary_rune_ids
///   Root(Data(CoreItems([3118],["Malignance"]),Runes(8100,[8112])))
/// Isto vira JSON usando os nomes de campo do cabeçalho.
pub fn parse_compact(text: &str) -> Result<Value> {
    let mut classes: HashMap<String, Vec<String>> = HashMap::new();
    let mut body = Vec::new();
    let mut header = true;
    for line in text.lines() {
        if header {
            if let Some((name, fields)) = line.strip_prefix("class ").and_then(|r| r.split_once(": ")) {
                classes.insert(name.to_string(), fields.split(',').map(String::from).collect());
                continue;
            }
            header = false;
        }
        body.push(line);
    }
    let src: Vec<char> = body.join("\n").chars().collect();
    Parser { src: &src, pos: 0, classes: &classes }.value()
}

struct Parser<'a> {
    src: &'a [char],
    pos: usize,
    classes: &'a HashMap<String, Vec<String>>,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }
    fn peek(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }
    fn err(&self) -> anyhow::Error {
        let near: String = self.src.iter().skip(self.pos).take(40).collect();
        anyhow!("resposta do OP.gg inválida perto de: {near}")
    }
    fn take_while(&mut self, f: impl Fn(char) -> bool) -> String {
        let start = self.pos;
        while self.peek().is_some_and(&f) {
            self.pos += 1;
        }
        self.src[start..self.pos].iter().collect()
    }

    fn list(&mut self, close: char) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        self.ws();
        if self.peek() == Some(close) {
            self.pos += 1;
            return Ok(out);
        }
        loop {
            out.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(c) if c == close => {
                    self.pos += 1;
                    return Ok(out);
                }
                _ => return Err(self.err()),
            }
        }
    }

    fn value(&mut self) -> Result<Value> {
        self.ws();
        let c = self.peek().ok_or_else(|| self.err())?;
        if c == '[' {
            self.pos += 1;
            return Ok(Value::Array(self.list(']')?));
        }
        if c == '"' {
            let start = self.pos;
            self.pos += 1;
            while let Some(ch) = self.peek() {
                if ch == '\\' {
                    self.pos += 2;
                    continue;
                }
                if ch == '"' {
                    break;
                }
                self.pos += 1;
            }
            self.pos += 1;
            let s: String = self.src[start..self.pos.min(self.src.len())].iter().collect();
            return serde_json::from_str(&s).map_err(|_| self.err());
        }
        if c == '-' || c.is_ascii_digit() {
            let s = self.take_while(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+' | 'e' | 'E'));
            return serde_json::from_str(&s).map_err(|_| self.err());
        }
        if c.is_alphanumeric() || c == '_' {
            let id = self.take_while(|ch| ch.is_alphanumeric() || ch == '_');
            self.ws();
            if self.peek() == Some('(') {
                self.pos += 1;
                let args = self.list(')')?;
                let fields = self.classes.get(&id);
                let mut obj = Map::new();
                for (k, a) in args.into_iter().enumerate() {
                    let name = fields.and_then(|f| f.get(k)).cloned().unwrap_or_else(|| k.to_string());
                    obj.insert(name, a);
                }
                return Ok(Value::Object(obj));
            }
            return Ok(match id.as_str() {
                "null" | "None" => Value::Null,
                "true" | "True" => Value::Bool(true),
                "false" | "False" => Value::Bool(false),
                _ => Value::String(id),
            });
        }
        Err(self.err())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compact_format() {
        let txt = "class Root: data\nclass Data: core_items,runes\nclass CoreItems: ids,ids_names\nclass Runes: primary_page_id,primary_rune_ids,win\n\nRoot(Data(CoreItems([3118,4645],[\"Malignance\",\"Zhonya's \\\"H\\\"\"]),Runes(8100,[8112,8139],0.51)))";
        let v = parse_compact(txt).unwrap();
        assert_eq!(v["data"]["core_items"]["ids"], json!([3118, 4645]));
        assert_eq!(v["data"]["core_items"]["ids_names"][1], "Zhonya's \"H\"");
        assert_eq!(v["data"]["runes"]["primary_page_id"], 8100);
        assert_eq!(v["data"]["runes"]["win"], 0.51);
    }

    #[tokio::test]
    #[ignore] // usa a rede: cargo test -- --ignored --nocapture
    async fn live_build() {
        let o = OpGg::new();
        let raw = o
            .call_tool("lol_get_champion_analysis", json!({"game_mode":"ranked","champion":"MONKEY_KING","position":"mid","desired_output_fields":["data.summary.positions[].name"]}))
            .await;
        println!("RAW: {raw:?}");
        let b = o.get_build(62, "ranked", None).await.unwrap();
        println!("{} {} {}", b.champ.name, b.position, b.data);
    }

    #[test]
    fn champion_names() {
        let c = Champ { id: 4, key: "TwistedFate".into(), name: "Twisted Fate".into() };
        assert_eq!(opgg_names(&c), vec!["TWISTED_FATE"]);
        let c = Champ { id: 897, key: "KSante".into(), name: "K'Sante".into() };
        assert_eq!(opgg_names(&c), vec!["KSANTE"]);
        let c = Champ { id: 62, key: "MonkeyKing".into(), name: "Wukong".into() };
        assert_eq!(opgg_names(&c), vec!["MONKEY_KING", "WUKONG"]);
    }
}
