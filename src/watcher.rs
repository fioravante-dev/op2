// Espera o cliente do LoL, escuta a seleção de campeão e aplica a build.
use crate::apply::{apply_items, apply_runes, apply_spells, remove_our_item_sets};
use crate::card;
use crate::lcu::{get_credentials, Lcu};
use crate::opgg::{matchup_build, Build, Champ, Counter, OpGg};
use crate::util::{log, Config};
use crate::UiEvent;
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::tungstenite::Message;

/// Canal para avisar a interface (bandeja). `None` no modo teste.
pub type Notify = Option<Arc<dyn Fn(UiEvent) + Send + Sync>>;

/// Comandos vindos do menu da bandeja e do cartão.
pub enum Cmd {
    /// Reaplica a build do campeão atual; Some(rota) força a rota (top/jungle/mid/adc/support).
    Reapply(Option<String>),
    /// Marca (sem confirmar) um campeão sugerido como counter
    Hover(i64),
    /// Troca para outra página de runas do matchup
    Runes(usize),
}

/// O que já foi feito nesta seleção de campeão (evita repetir chamadas ao OP.gg).
#[derive(Default)]
struct State {
    applied: Option<(i64, Option<i64>)>,
    counters_for: Option<i64>,
    matchup: Option<Matchup>,
}

struct Matchup {
    data: Value,
    champ: Champ,
    opponent: Champ,
    position: String,
}

#[derive(Clone)]
struct Ctx {
    lcu: Arc<Lcu>,
    opgg: Arc<OpGg>,
    cfg: Arc<Mutex<Config>>,
    notify: Notify,
    state: Arc<tokio::sync::Mutex<State>>,
}

impl Ctx {
    fn send(&self, e: UiEvent) {
        if let Some(n) = &self.notify {
            n(e);
        }
    }
    fn card_on(&self) -> bool {
        self.notify.is_some() && self.cfg.lock().unwrap().card
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

fn position_label(p: &str) -> &str {
    match p {
        "top" => "Top",
        "jungle" => "Jungle",
        "mid" => "Mid",
        "adc" => "ADC",
        "support" => "Suporte",
        other => other,
    }
}

struct Mode {
    /// modo no OP.gg: ranked, flex, aram, urf, nexus_blitz
    opgg: String,
    /// ARAM: Caos (tem melhorias)
    mayhem: bool,
}

/// Modo do OP.gg para a fila atual. `None` = sem suporte (Arena).
async fn detect_mode(lcu: &Lcu) -> Option<Mode> {
    let Ok(s) = lcu.get("/lol-gameflow/v1/session").await else {
        return Some(Mode { opgg: "ranked".into(), mayhem: false });
    };
    let q = &s["gameData"]["queue"];
    let mode = q["gameMode"].as_str().unwrap_or("");
    let opgg = if mode.contains("ARAM") || mode == "KIWI" {
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
    };
    Some(Mode { opgg: opgg.into(), mayhem: mode == "KIWI" })
}

fn win_rate(b: &Build) -> String {
    b.data["summary"]["average_stats"]["win_rate"]
        .as_f64()
        .map(|w| format!(" winrate {:.1}%", w * 100.0))
        .unwrap_or_default()
}

/// Aplica runas, feitiços e itens de uma build já buscada.
async fn apply_build(ctx: &Ctx, build: &Build, in_champ_select: bool, suffix: &str) -> Result<()> {
    let wr = win_rate(build);
    log(&format!("→ {} ({}/{}){suffix}{wr}", build.champ.name, build.game_mode, build.position));

    let cfg = ctx.cfg.lock().unwrap().clone();
    let me = ctx.lcu.get("/lol-summoner/v1/current-summoner").await?;
    let summoner_id = me["summonerId"].as_i64().unwrap_or(0);

    let report = |name: &str, r: Result<String>| {
        log(&format!("  {name}: {}", r.unwrap_or_else(|e| format!("ERRO {e}"))));
    };
    if cfg.runes {
        report("Runas", apply_runes(&ctx.lcu, build).await);
    }
    if cfg.spells && in_champ_select {
        report("Feitiços", apply_spells(&ctx.lcu, build, &cfg.flash_key).await);
    }
    if cfg.items {
        report("Itens", apply_items(&ctx.lcu, build, summoner_id).await);
    }
    ctx.send(UiEvent::LastBuild(format!("{} {}{suffix}{wr}", build.champ.name, build.label())));
    Ok(())
}

async fn apply_all(ctx: &Ctx, champion_id: i64, mode: &str, position: Option<String>, in_champ_select: bool) -> Result<Build> {
    let build = ctx.opgg.get_build(champion_id, mode, position).await?;
    apply_build(ctx, &build, in_champ_select, "").await?;
    Ok(build)
}

/// Build geral (sem inimigo conhecido) + cartão.
async fn apply_general(ctx: &Ctx, champion_id: i64, mode: &Mode, position: Option<String>, waiting_opponent: bool) {
    match apply_all(ctx, champion_id, &mode.opgg, position, true).await {
        Ok(b) if ctx.card_on() => {
            let augments = if mode.mayhem { ctx.opgg.aram_augments(&b.champ).await.unwrap_or_default() } else { Vec::new() };
            card::show(general_view(&b, &augments, waiting_opponent));
        }
        Ok(_) => {}
        Err(e) => log(&format!("Erro: {e}")),
    }
}

/// Build específica contra o inimigo da rota + cartão com as opções de runas.
async fn apply_matchup(ctx: &Ctx, champion_id: i64, opponent: &Champ, position: &str) -> Result<()> {
    let champ = ctx.opgg.champion(champion_id).await?;
    let mu = ctx.opgg.matchup(&champ, opponent, position).await?;
    let build = matchup_build(&mu, champ.clone(), position, 0);
    apply_build(ctx, &build, true, &format!(" vs {}", opponent.name)).await?;
    if ctx.card_on() {
        card::show(matchup_view(&mu, &champ, opponent, position, 0));
    }
    ctx.state.lock().await.matchup = Some(Matchup { data: mu, champ, opponent: opponent.clone(), position: position.into() });
    Ok(())
}

fn url_key(c: &Champ) -> String {
    c.key.to_lowercase()
}

fn pct(v: f64) -> String {
    format!("{:.0}%", v * 100.0)
}

fn counters_view(enemy: &Champ, position: &str, list: &[Counter]) -> card::View {
    card::View {
        title: format!("Counters para {}", enemy.name),
        subtitle: format!("Inimigo da sua rota ({}) · clique para marcar", position_label(position)),
        sections: vec![(
            "Melhores escolhas".into(),
            list.iter()
                .map(|c| card::Row {
                    text: c.name.clone(),
                    detail: format!("vence {}", pct(c.win_rate)),
                    action: Some(card::Action::Hover(c.id)),
                    ..Default::default()
                })
                .collect(),
        )],
        link: Some(format!("https://op.gg/lol/champions/{}/counters/{position}", url_key(enemy))),
    }
}

fn rune_label(r: &Value) -> String {
    let keystone = r["primary_rune_names"][0].as_str().unwrap_or("Runas");
    match r["secondary_page_name"].as_str() {
        Some(sec) => format!("{keystone} + {sec}"),
        None => keystone.to_string(),
    }
}

fn matchup_view(mu: &Value, champ: &Champ, opponent: &Champ, position: &str, selected: usize) -> card::View {
    let d = &mu["data"];
    let advantage = match d["lane_advantage_champion"].as_str() {
        Some(n) if n == opponent.name => format!("{} tem vantagem na lane", opponent.name),
        Some(n) if n == champ.name => "Você tem vantagem na lane".into(),
        _ => "Lane equilibrada".into(),
    };
    let pages: Vec<&Value> = d["runes"].as_array().into_iter().flatten().take(4).collect();
    let names = |r: &Value| -> Vec<String> {
        ["primary_rune_names", "secondary_rune_names"]
            .iter()
            .flat_map(|k| r[*k].as_array().into_iter().flatten().filter_map(|n| n.as_str().map(String::from)))
            .collect()
    };
    let runes = pages
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let (win, play) = (r["win"].as_f64().unwrap_or(0.0), r["play"].as_f64().unwrap_or(0.0));
            let wr = if play > 0.0 { format!(" · WR {}", pct(win / play)) } else { String::new() };
            // duas páginas com a mesma runa principal: mostra a runa que muda
            let mut text = rune_label(r);
            if let Some(prev) = pages[..i].iter().find(|p| rune_label(p) == text) {
                let before = names(prev);
                if let Some(diff) = names(r).into_iter().find(|n| !before.contains(n)) {
                    text = format!("{text} · {diff}");
                }
            }
            card::Row {
                text,
                detail: format!("{}{wr}", pct(r["pick_rate"].as_f64().unwrap_or(0.0))),
                marker: Some(i == selected),
                action: Some(card::Action::Runes(i)),
                ..Default::default()
            }
        })
        .collect();
    let mut sections = vec![("Runas para este matchup".to_string(), runes)];
    if let Some(tip) = d["opponent_champion_tip"].as_str().filter(|t| !t.is_empty()) {
        sections.push((
            format!("Dica contra {}", opponent.name),
            vec![card::Row { text: tip.into(), wrap: true, ..Default::default() }],
        ));
    }
    card::View {
        title: format!("{} vs {}", champ.name, opponent.name),
        subtitle: format!("{advantage} · {}", position_label(position)),
        sections,
        link: Some(format!(
            "https://op.gg/lol/champions/{}/build/{position}?target_champion={}",
            url_key(champ),
            url_key(opponent)
        )),
    }
}

fn general_view(b: &Build, augments: &[(String, f64)], waiting_opponent: bool) -> card::View {
    let r = &b.data["runes"];
    let mut subtitle = win_rate(b).trim().replace("winrate", "Winrate");
    if waiting_opponent {
        subtitle = format!("{subtitle} · aguardando o inimigo da sua rota");
    }
    let mut sections = Vec::new();
    if !r.is_null() {
        sections.push((
            "Runas".to_string(),
            vec![card::Row {
                text: rune_label(r),
                detail: r["pick_rate"].as_f64().map(pct).unwrap_or_default(),
                marker: Some(true),
                ..Default::default()
            }],
        ));
    }
    if !augments.is_empty() {
        sections.push((
            "Melhorias mais escolhidas".to_string(),
            augments.iter().map(|(n, p)| card::Row { text: n.clone(), detail: pct(*p), ..Default::default() }).collect(),
        ));
    }
    let key = url_key(&b.champ);
    let link = if b.game_mode == "aram" {
        format!("https://op.gg/lol/modes/aram/{key}/build")
    } else {
        format!("https://op.gg/lol/champions/{key}/build/{}", b.position)
    };
    card::View { title: format!("{} · {}", b.champ.name, b.label()), subtitle, sections, link: Some(link) }
}

/// O que importa da seleção de campeão.
struct Snap {
    champ: i64,
    /// já confirmou o campeão (no ARAM, assim que recebe um)
    locked: bool,
    position: Option<String>,
    enemies: Vec<i64>,
}

fn my_cell(s: &Value) -> Option<&Value> {
    let cell = &s["localPlayerCellId"];
    s["myTeam"].as_array().and_then(|t| t.iter().find(|m| &m["cellId"] == cell))
}

/// Minha ação de escolha (pick) ainda em aberto, se houver.
fn my_open_pick(s: &Value) -> Option<&Value> {
    let cell = &s["localPlayerCellId"];
    s["actions"]
        .as_array()?
        .iter()
        .flat_map(|turn| turn.as_array().into_iter().flatten())
        .find(|a| &a["actorCellId"] == cell && a["type"] == "pick" && a["completed"] != true)
}

fn snapshot(s: &Value) -> Option<Snap> {
    let me = my_cell(s)?;
    let champ = me["championId"].as_i64().unwrap_or(0);
    let mut enemies: Vec<i64> = s["theirTeam"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["championId"].as_i64())
        .filter(|&id| id > 0)
        .collect();
    enemies.sort();
    Some(Snap {
        champ,
        locked: champ > 0 && my_open_pick(s).is_none(),
        position: me["assignedPosition"].as_str().and_then(map_position),
        enemies,
    })
}

async fn handle(ctx: Ctx, snap: Snap) {
    let Some(mode) = detect_mode(&ctx.lcu).await else {
        if snap.locked {
            log("Modo de jogo sem suporte (Arena). Ignorando.");
        }
        return;
    };
    let lane = mode.opgg != "aram";
    let opponent = match (&snap.position, lane) {
        (Some(p), true) => ctx.opgg.lane_opponent(&snap.enemies, p).await,
        _ => None,
    };

    if !snap.locked {
        // antes de confirmar: counters contra o inimigo da rota
        let (Some(enemy), Some(pos)) = (opponent, snap.position.as_deref()) else { return };
        if !ctx.card_on() {
            return;
        }
        {
            let mut st = ctx.state.lock().await;
            if st.counters_for == Some(enemy.id) {
                return;
            }
            st.counters_for = Some(enemy.id);
        }
        match ctx.opgg.counters(&enemy, pos).await {
            Ok(list) if !list.is_empty() => card::show(counters_view(&enemy, pos, &list)),
            Ok(_) => {}
            Err(e) => log(&format!("Counters indisponíveis: {e}")),
        }
        return;
    }

    let key = (snap.champ, opponent.as_ref().map(|c| c.id));
    {
        let mut st = ctx.state.lock().await;
        if st.applied == Some(key) {
            return;
        }
        st.applied = Some(key);
        st.matchup = None;
    }
    if let (Some(opp), Some(pos)) = (&opponent, &snap.position) {
        match apply_matchup(&ctx, snap.champ, opp, pos).await {
            Ok(()) => return,
            Err(e) => log(&format!("  Matchup vs {} indisponível ({e}); usando a build geral", opp.name)),
        }
    }
    apply_general(&ctx, snap.champ, &mode, snap.position.clone(), lane && snap.position.is_some()).await;
}

/// Segue a sessão de seleção de campeão e reage quando algo relevante muda.
struct Tracker {
    last_key: Option<String>,
    generation: Arc<AtomicU64>,
}

impl Tracker {
    fn on_session(&mut self, s: &Value, ctx: &Ctx) {
        let Some(snap) = snapshot(s) else { return };
        if snap.champ == 0 && snap.enemies.is_empty() {
            return;
        }
        let key = format!("{}:{}:{}:{:?}", s["gameId"], snap.champ, snap.locked, snap.enemies);
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
            if generation.load(Ordering::SeqCst) == my_gen {
                handle(ctx, snap).await;
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

/// Saiu da seleção de campeão: esconde o cartão e esquece o que foi feito.
async fn end_champ_select(ctx: &Ctx, tracker: &mut Tracker) {
    tracker.last_key = None;
    tracker.generation.fetch_add(1, Ordering::SeqCst);
    *ctx.state.lock().await = State::default();
    card::hide();
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
                    end_champ_select(&ctx, &mut tracker).await;
                    continue;
                }
                tracker.on_session(&ev["data"], &ctx);
            }
            Some(cmd) = cmds.recv() => match cmd {
                Cmd::Reapply(position) => reapply(&ctx, &mut tracker, position).await,
                Cmd::Hover(id) => {
                    let ctx = ctx.clone();
                    tokio::spawn(async move { hover(&ctx, id).await });
                }
                Cmd::Runes(i) => {
                    let ctx = ctx.clone();
                    tokio::spawn(async move { switch_runes(&ctx, i).await });
                }
            },
        }
    }
    end_champ_select(&ctx, &mut tracker).await;
    Ok(())
}

/// Clique num counter do cartão: marca o campeão na seleção (sem confirmar).
async fn hover(ctx: &Ctx, champion_id: i64) {
    let result = async {
        let s = ctx.lcu.get("/lol-champ-select/v1/session").await?;
        let action = my_open_pick(&s).ok_or_else(|| anyhow::anyhow!("não há escolha em aberto"))?;
        ctx.lcu
            .patch(&format!("/lol-champ-select/v1/session/actions/{}", action["id"]), &json!({ "championId": champion_id }))
            .await?;
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = result {
        log(&format!("Não deu para marcar o campeão: {e}"));
        ctx.send(UiEvent::Info("Não deu para marcar esse campeão".into()));
    }
}

/// Clique numa página de runas do cartão.
async fn switch_runes(ctx: &Ctx, idx: usize) {
    let st = ctx.state.lock().await;
    let Some(m) = &st.matchup else { return };
    let build = matchup_build(&m.data, m.champ.clone(), &m.position, idx);
    match apply_runes(&ctx.lcu, &build).await {
        Ok(r) => log(&format!("  Runas (opção {}): {r}", idx + 1)),
        Err(e) => log(&format!("  Runas (opção {}): ERRO {e}", idx + 1)),
    }
    card::show(matchup_view(&m.data, &m.champ, &m.opponent, &m.position, idx));
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
    let Some(snap) = snapshot(&s).filter(|sn| sn.champ > 0) else {
        return info("Reaplicar: escolha um campeão primeiro.");
    };
    // cancela qualquer aplicação automática pendente e esquece o que já foi aplicado
    tracker.last_key = Some(format!("{}:{}:{}:{:?}", s["gameId"], snap.champ, snap.locked, snap.enemies));
    tracker.generation.fetch_add(1, Ordering::SeqCst);
    *ctx.state.lock().await = State::default();
    info("Reaplicando build...");
    let ctx = ctx.clone();
    tokio::spawn(async move {
        match position {
            // rota escolhida no menu: build geral daquela rota
            Some(pos) => match detect_mode(&ctx.lcu).await {
                None => log("Modo de jogo sem suporte (Arena). Ignorando."),
                Some(mode) => apply_general(&ctx, snap.champ, &mode, Some(pos), false).await,
            },
            None => handle(ctx, Snap { locked: true, ..snap }).await,
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
                    let ctx = Ctx {
                        lcu: Arc::new(lcu),
                        opgg: opgg.clone(),
                        cfg: cfg.clone(),
                        notify: notify.clone(),
                        state: Default::default(),
                    };
                    log(&format!("Conectado ao cliente (porta {}).", ctx.lcu.port()));
                    let patch = check_patch(&ctx).await;
                    ctx.send(UiEvent::Connected(Some(patch.unwrap_or_default())));
                    if let Err(e) = session(ctx.clone(), &mut cmds).await {
                        log(&format!("Conexão encerrada: {e}"));
                    }
                    card::hide();
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

fn test_ctx(lcu: Lcu, opgg: Arc<OpGg>) -> Ctx {
    Ctx { lcu: Arc::new(lcu), opgg, cfg: Arc::new(Mutex::new(Config::load())), notify: None, state: Default::default() }
}

/// `op2 teste <campeão> [rota] [inimigo]`: aplica runas e itens agora, sem seleção de campeão.
/// Com inimigo, usa o guia de matchup do OP.gg.
pub async fn test_mode(champion: String, position: Option<String>, opponent: Option<String>, apply: bool) -> Result<Vec<card::View>> {
    let creds = get_credentials(true).ok_or_else(|| anyhow::anyhow!("Abra o cliente do LoL primeiro."))?;
    let opgg = Arc::new(OpGg::new());
    let champ = opgg.find_champion(&champion).await?;
    let ctx = test_ctx(Lcu::new(creds)?, opgg.clone());
    if let Some(opp) = opponent {
        let opp = opgg.find_champion(&opp).await?;
        let pos = position.unwrap_or_else(|| "mid".into());
        let mu = opgg.matchup(&champ, &opp, &pos).await?;
        let build = matchup_build(&mu, champ.clone(), &pos, 0);
        if apply {
            apply_build(&ctx, &build, false, &format!(" vs {}", opp.name)).await?;
        }
        let counters = opgg.counters(&opp, &pos).await?;
        let views = vec![counters_view(&opp, &pos, &counters), matchup_view(&mu, &champ, &opp, &pos, 0)];
        views.iter().for_each(print_view);
        return Ok(views);
    }
    let b = if apply {
        apply_all(&ctx, champ.id, "ranked", position, false).await?
    } else {
        opgg.get_build(champ.id, "ranked", position).await?
    };
    let augments = opgg.aram_augments(&b.champ).await.unwrap_or_default();
    let v = general_view(&b, &augments, false);
    print_view(&v);
    Ok(vec![v])
}

fn print_view(v: &card::View) {
    println!("\n[{}]  {}", v.title, v.subtitle);
    for (h, rows) in &v.sections {
        println!("  {h}");
        for r in rows {
            let m = match r.marker {
                Some(true) => "● ",
                Some(false) => "○ ",
                None => "",
            };
            println!("    {m}{}  {}", r.text, r.detail);
        }
    }
    if let Some(l) = &v.link {
        println!("  {l}");
    }
}
