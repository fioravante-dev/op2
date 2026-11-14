// Espera o cliente do LoL, escuta a seleção de campeão e aplica a build.
use crate::apply::{apply_items, apply_runes, apply_spells, remove_our_item_sets};
use crate::lcu::{get_credentials, Lcu};
use crate::opgg::OpGg;
use crate::util::{log, Config};
use crate::UiEvent;
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::tungstenite::Message;

/// Canal para avisar a interface (bandeja). `None` no modo teste.
pub type Notify = Option<Arc<dyn Fn(UiEvent) + Send + Sync>>;

/// Comandos vindos do menu da bandeja.
pub enum Cmd {
    /// Reaplica a build do campeão atual; Some(rota) força a rota (top/jungle/mid/adc/support).
    Reapply(Option<String>),
}

#[derive(Clone)]
struct Ctx {
    lcu: Arc<Lcu>,
    opgg: Arc<OpGg>,
    cfg: Arc<Mutex<Config>>,
    notify: Notify,
}

impl Ctx {
    fn send(&self, e: UiEvent) {
        if let Some(n) = &self.notify {
            n(e);
        }
    }
}

fn map_position(p: &str) -> Option<String> {
    Some(
        match p {
            "top" => "top",
            "jungle" => "jungle",
            "middle" => "mid",
            "bottom" => "adc",
            "utility" => "support",
            _ => return None,
        }
        .into(),
    )
}

/// Modo do OP.gg para a fila atual. `None` = sem suporte (Arena).
async fn detect_mode(lcu: &Lcu) -> Option<String> {
    let Ok(s) = lcu.get("/lol-gameflow/v1/session").await else { return Some("ranked".into()) };
    let q = &s["gameData"]["queue"];
    let mode = q["gameMode"].as_str().unwrap_or("");
    Some(
        if mode.contains("ARAM") || mode == "KIWI" {
            "aram" // KIWI = ARAM: Caos
        } else if mode.contains("URF") {
            "urf"
        } else if mode == "NEXUSBLITZ" {
            "nexus_blitz"
        } else if mode == "CHERRY" {
            return None; // Arena
        } else if q["id"].as_i64() == Some(440) {
            "flex"
        } else {
            "ranked"
        }
        .into(),
    )
}

async fn apply_all(ctx: &Ctx, champion_id: i64, mode: &str, position: Option<String>, in_champ_select: bool) -> Result<()> {
    let build = ctx.opgg.get_build(champion_id, mode, position).await?;
    let wr = build.data["summary"]["average_stats"]["win_rate"]
        .as_f64()
        .map(|w| format!(" winrate {:.1}%", w * 100.0))
        .unwrap_or_default();
    log(&format!("→ {} ({}/{}){wr}", build.champ.name, build.game_mode, build.position));

    let cfg = ctx.cfg.lock().unwrap().clone();
    let me = ctx.lcu.get("/lol-summoner/v1/current-summoner").await?;
    let summoner_id = me["summonerId"].as_i64().unwrap_or(0);

    let report = |name: &str, r: Result<String>| {
        log(&format!("  {name}: {}", r.unwrap_or_else(|e| format!("ERRO {e}"))));
    };
    if cfg.runes {
        report("Runas", apply_runes(&ctx.lcu, &build).await);
    }
    if cfg.spells && in_champ_select {
        report("Feitiços", apply_spells(&ctx.lcu, &build, &cfg.flash_key).await);
    }
    if cfg.items {
        report("Itens", apply_items(&ctx.lcu, &build, summoner_id).await);
    }
    ctx.send(UiEvent::LastBuild(format!("{} {}{wr}", build.champ.name, build.label())));
    Ok(())
}

/// Segue a sessão de seleção de campeão e dispara a aplicação quando o campeão muda.
struct Tracker {
    last_key: Option<String>,
    generation: Arc<AtomicU64>,
}

impl Tracker {
    fn on_session(&mut self, s: &Value, ctx: &Ctx) {
        let cell = &s["localPlayerCellId"];
        let Some(me) = s["myTeam"].as_array().and_then(|t| t.iter().find(|m| &m["cellId"] == cell)) else { return };
        let champion_id = me["championId"].as_i64().unwrap_or(0);
        if champion_id == 0 {
            return;
        }
        let position = me["assignedPosition"].as_str().and_then(map_position);
        let key = format!("{}:{}", s["gameId"], champion_id);
        if self.last_key.as_deref() == Some(key.as_str()) {
            return;
        }
        self.last_key = Some(key);

        // espera um pouco: evita aplicar várias vezes se o jogador troca rápido de campeão.
        // Na reta final da seleção (troca de última hora no ARAM) aplica na hora.
        let timer = &s["timer"];
        let ending = timer["phase"] == "FINALIZATION"
            && timer["adjustedTimeLeftInPhase"].as_f64().is_some_and(|ms| ms < 10_000.0);
        let delay = if ending { 0 } else { 800 };
        let my_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let generation = self.generation.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            if generation.load(Ordering::SeqCst) != my_gen {
                return;
            }
            match detect_mode(&ctx.lcu).await {
                None => log("Modo de jogo sem suporte (Arena). Ignorando."),
                Some(mode) => {
                    if let Err(e) = apply_all(&ctx, champion_id, &mode, position, true).await {
                        log(&format!("Erro: {e}"));
                    }
                }
            }
        });
    }
}

/// Lê o patch atual do cliente ("16.22.8348910+branch..." -> "16.22").
async fn current_patch(lcu: &Lcu) -> Option<String> {
    let v = lcu.get("/lol-patch/v1/game-version").await.ok()?;
    let mut parts = v.as_str()?.split('.');
    Some(format!("{}.{}", parts.next()?, parts.next()?))
}

/// Ao conectar: se o LoL mudou de patch desde a última vez, recarrega a lista de
/// campeões e apaga os sets de itens antigos do OP2.
async fn check_patch(ctx: &Ctx) -> Option<String> {
    let patch = current_patch(&ctx.lcu).await?;
    let previous = ctx.cfg.lock().unwrap().last_patch.clone();
    if previous != patch {
        if previous.is_empty() {
            log(&format!("Patch do LoL: {patch}"));
        } else {
            log(&format!("Novo patch detectado: {previous} → {patch}. Atualizando dados..."));
            ctx.opgg.refresh_champions().await;
            if let Ok(me) = ctx.lcu.get("/lol-summoner/v1/current-summoner").await {
                match remove_our_item_sets(&ctx.lcu, me["summonerId"].as_i64().unwrap_or(0)).await {
                    Ok(n) => log(&format!("  {n} set(s) de itens do patch anterior removido(s)")),
                    Err(e) => log(&format!("  Erro ao limpar sets antigos: {e}")),
                }
            }
        }
        let mut cfg = ctx.cfg.lock().unwrap();
        cfg.last_patch = patch.clone();
        cfg.save();
    }
    Some(patch)
}

async fn session(ctx: Ctx, cmds: &mut UnboundedReceiver<Cmd>) -> Result<()> {
    let mut ws = ctx.lcu.connect_ws().await?;
    ws.send(Message::Text(r#"[5,"OnJsonApiEvent_lol-champ-select_v1_session"]"#.into())).await?;

    let mut tracker = Tracker { last_key: None, generation: Arc::new(AtomicU64::new(0)) };
    if let Ok(s) = ctx.lcu.get("/lol-champ-select/v1/session").await {
        tracker.on_session(&s, &ctx); // já estava em champ select ao conectar
    }
    loop {
        tokio::select! {
            msg = ws.next() => {
                let Some(msg) = msg else { break };
                let text = match msg? {
                    Message::Text(t) => t,
                    Message::Close(_) => break,
                    _ => continue,
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                if v[0] != 8 {
                    continue;
                }
                let ev = &v[2];
                if ev["eventType"] == "Delete" {
                    tracker.last_key = None; // saiu da seleção de campeão
                    continue;
                }
                tracker.on_session(&ev["data"], &ctx);
            }
            Some(cmd) = cmds.recv() => match cmd {
                Cmd::Reapply(position) => reapply(&ctx, &mut tracker, position).await,
            },
        }
    }
    Ok(())
}

/// "Reaplicar build agora" do menu: força a aplicação para o campeão atual.
async fn reapply(ctx: &Ctx, tracker: &mut Tracker, position: Option<String>) {
    let info = |m: &str| {
        log(m);
        ctx.send(UiEvent::Info(m.into()));
    };
    let Ok(s) = ctx.lcu.get("/lol-champ-select/v1/session").await else {
        return info("Reaplicar: você não está na seleção de campeão.");
    };
    let cell = &s["localPlayerCellId"];
    let me = s["myTeam"].as_array().and_then(|t| t.iter().find(|m| &m["cellId"] == cell));
    let champion_id = me.and_then(|m| m["championId"].as_i64()).unwrap_or(0);
    if champion_id == 0 {
        return info("Reaplicar: escolha um campeão primeiro.");
    }
    let position = position.or_else(|| me.and_then(|m| m["assignedPosition"].as_str()).and_then(map_position));
    // marca como aplicado e cancela qualquer aplicação automática pendente
    tracker.last_key = Some(format!("{}:{}", s["gameId"], champion_id));
    tracker.generation.fetch_add(1, Ordering::SeqCst);
    info("Reaplicando build...");
    let ctx = ctx.clone();
    tokio::spawn(async move {
        match detect_mode(&ctx.lcu).await {
            None => log("Modo de jogo sem suporte (Arena). Ignorando."),
            Some(mode) => {
                if let Err(e) = apply_all(&ctx, champion_id, &mode, position, true).await {
                    log(&format!("Erro: {e}"));
                    ctx.send(UiEvent::Info(format!("Erro: {e}")));
                }
            }
        }
    });
}

pub async fn run(cfg: Arc<Mutex<Config>>, notify: Notify, mut cmds: UnboundedReceiver<Cmd>) {
    log(&format!("OP2 v{} iniciado. Aguardando o cliente do LoL...", crate::update::VERSION));
    let opgg = Arc::new(OpGg::new());
    {
        let o = opgg.clone();
        tokio::spawn(async move {
            let _ = o.champions().await; // pré-carrega a lista de campeões
        });
    }
    let mut tick: u32 = 0;
    loop {
        // fallback pela linha de comando do processo só a cada ~30s (chama o PowerShell)
        if let Some(creds) = get_credentials(tick % 10 == 0) {
            if let Ok(lcu) = Lcu::new(creds) {
                if lcu.get("/lol-summoner/v1/current-summoner").await.is_ok() {
                    let ctx = Ctx { lcu: Arc::new(lcu), opgg: opgg.clone(), cfg: cfg.clone(), notify: notify.clone() };
                    log(&format!("Conectado ao cliente (porta {}).", ctx.lcu.port()));
                    let patch = check_patch(&ctx).await;
                    ctx.send(UiEvent::Connected(Some(patch.unwrap_or_default())));
                    if let Err(e) = session(ctx.clone(), &mut cmds).await {
                        log(&format!("Conexão encerrada: {e}"));
                    }
                    ctx.send(UiEvent::Connected(None));
                    log("Cliente desconectado. Aguardando...");
                }
            }
        }
        tick = tick.wrapping_add(1);

        // espera 3s até tentar de novo, respondendo cliques no menu enquanto isso
        let wait = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(wait);
        loop {
            tokio::select! {
                _ = &mut wait => break,
                Some(_) = cmds.recv() => {
                    log("Reaplicar: o cliente do LoL não está aberto.");
                    if let Some(n) = &notify {
                        n(UiEvent::Info("O cliente do LoL não está aberto".into()));
                    }
                }
            }
        }
    }
}

/// `op2 teste <campeão> [rota]`: aplica runas e itens agora, sem seleção de campeão.
pub async fn test_mode(champion: String, position: Option<String>) -> Result<()> {
    let creds = get_credentials(true).ok_or_else(|| anyhow::anyhow!("Abra o cliente do LoL primeiro."))?;
    let opgg = Arc::new(OpGg::new());
    let champ = opgg.find_champion(&champion).await?;
    let ctx = Ctx { lcu: Arc::new(Lcu::new(creds)?), opgg, cfg: Arc::new(Mutex::new(Config::load())), notify: None };
    apply_all(&ctx, champ.id, "ranked", position, false).await
}
