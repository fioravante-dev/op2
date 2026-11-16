// OP2: ícone na bandeja que aplica runas, feitiços e itens do OP.gg no cliente do LoL.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apply;
mod card;
mod lcu;
mod opgg;
mod update;
mod util;
mod watcher;

use std::sync::{Arc, Mutex};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tokio::sync::mpsc::unbounded_channel;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};
use util::{log, Config};
use watcher::Cmd;

pub enum UiEvent {
    /// Some(patch) = conectado; None = aguardando o cliente
    Connected(Option<String>),
    LastBuild(String),
    /// Mensagem curta mostrada no menu/tooltip
    Info(String),
    UpdateAvailable(String),
    Exit,
    Menu(MenuEvent),
}

/// Ícone "O" da bandeja: verde = conectado, cinza = aguardando.
fn make_icon(connected: bool) -> Icon {
    const S: u32 = 32;
    let (r, g, b) = if connected { (0x2b, 0xc4, 0x8a) } else { (0x8a, 0x8f, 0x98) };
    let mut rgba = Vec::with_capacity((S * S * 4) as usize);
    for y in 0..S {
        for x in 0..S {
            let (dx, dy) = (x as f32 - 15.5, y as f32 - 15.5);
            let d = (dx * dx + dy * dy).sqrt();
            let outer = (15.5 - d).clamp(0.0, 1.0);
            let inner = (d - 7.0).clamp(0.0, 1.0);
            rgba.extend_from_slice(&[r, g, b, (outer.min(inner) * 255.0) as u8]);
        }
    }
    Icon::from_rgba(rgba, S, S).expect("icon")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // `op2 teste <campeão> [rota] [inimigo]` aplica agora; `op2 cartao ...` também mostra o cartão
    let cmd = args.get(1).map(String::as_str);
    if cmd == Some("teste") || cmd == Some("cartao") {
        let champ = args.get(2).cloned().unwrap_or_else(|| "Ahri".into());
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        match rt.block_on(watcher::test_mode(champ, args.get(3).cloned(), args.get(4).cloned(), cmd == Some("teste"))) {
            Ok(views) if cmd == Some("cartao") => {
                card::start(|_| {});
                for v in views {
                    card::show(v);
                    std::thread::sleep(std::time::Duration::from_secs(6));
                }
            }
            Ok(_) => {}
            Err(e) => log(&format!("Erro: {e}")),
        }
        return;
    }
    let after_restart = args.iter().any(|a| a == "--wait");

    // O .exe baixado funciona como instalador: copia para a pasta do app, cria o atalho e abre.
    if !cfg!(debug_assertions) && !after_restart && !update::is_installed_copy() {
        match update::install_and_relaunch() {
            Ok(dir) => util::message_box(&format!(
                "OP2 v{} instalado!\n\nAtalho criado na área de trabalho.\nO app já está rodando na bandeja (ícone \"O\" perto do relógio).\n\nPasta: {}",
                update::VERSION,
                dir.display()
            )),
            Err(e) => util::message_box(&format!("Falha ao instalar o OP2: {e}")),
        }
        return;
    }

    if !util::single_instance(after_restart) {
        util::message_box("O OP2 já está aberto.\n\nProcure o ícone \"O\" na bandeja, perto do relógio (pode estar na setinha ^).");
        return;
    }

    let cfg = Arc::new(Mutex::new(Config::load()));
    let event_loop = EventLoopBuilder::<UiEvent>::with_user_event().build();

    // EventLoopProxy é Send mas não Sync; o Mutex deixa compartilhar entre threads.
    let proxy = Mutex::new(event_loop.create_proxy());
    let send: Arc<dyn Fn(UiEvent) + Send + Sync> = Arc::new(move |e: UiEvent| {
        let _ = proxy.lock().unwrap().send_event(e);
    });
    {
        let s = send.clone();
        MenuEvent::set_event_handler(Some(move |e| s(UiEvent::Menu(e))));
        TrayIconEvent::set_event_handler(Some(|_| {})); // descarta eventos de mouse
    }

    let (cmd_tx, cmd_rx) = unbounded_channel::<Cmd>();
    let (update_tx, update_rx) = unbounded_channel::<()>();
    {
        let tx = cmd_tx.clone();
        card::start(move |a| {
            let _ = tx.send(match a {
                card::Action::Hover(id) => Cmd::Hover(id),
                card::Action::Runes(i) => Cmd::Runes(i),
                card::Action::Opponent(id) => Cmd::Opponent(id),
            });
        });
    }
    {
        let cfg = cfg.clone();
        let s = send.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("runtime");
            rt.spawn(update::run(s.clone(), update_rx));
            rt.block_on(watcher::run(cfg, Some(s), cmd_rx));
        });
    }

    let c = cfg.lock().unwrap().clone();
    let title = MenuItem::new(format!("OP2 v{}", update::VERSION), false, None);
    let status = MenuItem::new("Aguardando o cliente do LoL...", false, None);
    let last = MenuItem::new("Nenhuma build aplicada ainda", false, None);
    let reapply = MenuItem::new("Reaplicar build agora", true, None);
    let pos_items: Vec<(MenuItem, &str)> = [("Top", "top"), ("Jungle", "jungle"), ("Mid", "mid"), ("ADC", "adc"), ("Suporte", "support")]
        .into_iter()
        .map(|(label, pos)| (MenuItem::new(label, true, None), pos))
        .collect();
    let reapply_as = Submenu::with_items(
        "Reaplicar como...",
        true,
        &pos_items.iter().map(|(m, _)| m as &dyn tray_icon::menu::IsMenuItem).collect::<Vec<_>>(),
    )
    .expect("menu");
    let runes = CheckMenuItem::new("Runas", true, c.runes, None);
    let spells = CheckMenuItem::new("Feitiços", true, c.spells, None);
    let items = CheckMenuItem::new("Itens", true, c.items, None);
    let card_item = CheckMenuItem::new("Cartão com counters e builds", true, c.card, None);
    let flash_auto = CheckMenuItem::new("Manter onde está", true, c.flash_key == "auto", None);
    let flash_d = CheckMenuItem::new("Sempre no D", true, c.flash_key == "D", None);
    let flash_f = CheckMenuItem::new("Sempre no F", true, c.flash_key == "F", None);
    let flash = Submenu::with_items("Flash", true, &[&flash_auto, &flash_d, &flash_f]).expect("menu");
    let update_item = MenuItem::new("Verificar atualizações", true, None);
    let open_log = MenuItem::new("Abrir log", true, None);
    let quit = MenuItem::new("Sair", true, None);
    let menu = Menu::with_items(&[
        &title,
        &status,
        &last,
        &PredefinedMenuItem::separator(),
        &reapply,
        &reapply_as,
        &PredefinedMenuItem::separator(),
        &runes,
        &spells,
        &items,
        &flash,
        &card_item,
        &PredefinedMenuItem::separator(),
        &update_item,
        &open_log,
        &quit,
    ])
    .expect("menu");

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("OP2 - aguardando o cliente do LoL")
        .with_icon(make_icon(false))
        .build()
        .expect("tray");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        let Event::UserEvent(ev) = event else { return };
        match ev {
            UiEvent::Connected(patch) => {
                let txt = match &patch {
                    Some(p) if !p.is_empty() => format!("Conectado ao LoL (patch {p})"),
                    Some(_) => "Conectado ao LoL".into(),
                    None => "Aguardando o cliente do LoL...".into(),
                };
                status.set_text(&txt);
                let _ = tray.set_icon(Some(make_icon(patch.is_some())));
                let _ = tray.set_tooltip(Some(format!("OP2 - {txt}")));
            }
            UiEvent::LastBuild(s) => {
                last.set_text(format!("Última: {s}"));
                let _ = tray.set_tooltip(Some(format!("OP2 - {s}")));
            }
            UiEvent::Info(s) => {
                if update_item.text().starts_with("Verificando") {
                    update_item.set_text("Verificar atualizações");
                }
                last.set_text(&s);
                let _ = tray.set_tooltip(Some(format!("OP2 - {s}")));
            }
            UiEvent::UpdateAvailable(v) => update_item.set_text(format!("Atualizar para v{v}")),
            UiEvent::Exit => *control_flow = ControlFlow::Exit,
            UiEvent::Menu(e) => {
                let id = &e.id;
                if id == reapply.id() {
                    let _ = cmd_tx.send(Cmd::Reapply(None));
                    return;
                }
                if let Some((_, pos)) = pos_items.iter().find(|(m, _)| id == m.id()) {
                    let _ = cmd_tx.send(Cmd::Reapply(Some(pos.to_string())));
                    return;
                }
                if id == update_item.id() {
                    update_item.set_text("Verificando atualizações...");
                    let _ = update_tx.send(());
                    return;
                }
                if id == open_log.id() {
                    let _ = std::process::Command::new("notepad").arg(util::log_path()).spawn();
                    return;
                }
                if id == quit.id() {
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                let mut c = cfg.lock().unwrap();
                if id == runes.id() {
                    c.runes = runes.is_checked();
                } else if id == spells.id() {
                    c.spells = spells.is_checked();
                } else if id == items.id() {
                    c.items = items.is_checked();
                } else if id == card_item.id() {
                    c.card = card_item.is_checked();
                    if !c.card {
                        card::hide();
                    }
                } else if id == flash_auto.id() || id == flash_d.id() || id == flash_f.id() {
                    c.flash_key = if id == flash_d.id() {
                        "D"
                    } else if id == flash_f.id() {
                        "F"
                    } else {
                        "auto"
                    }
                    .into();
                    flash_auto.set_checked(c.flash_key == "auto");
                    flash_d.set_checked(c.flash_key == "D");
                    flash_f.set_checked(c.flash_key == "F");
                } else {
                    return;
                }
                c.save();
            }
        }
    });
}
