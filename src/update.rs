// Instalação (o próprio .exe é o instalador) e atualização automática via GitHub Releases.
use crate::util::log;
use crate::UiEvent;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

/// Repositório no GitHub ("dono/nome"). Enquanto for "OWNER/...", a atualização fica desligada.
pub const REPO: &str = "fioravante-dev/op2";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ---------- instalação ----------

pub fn install_dir() -> PathBuf {
    PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into())).join("Programs").join("OP2")
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
        _ => false,
    }
}

/// true se este .exe é a cópia instalada (e não o arquivo baixado).
pub fn is_installed_copy() -> bool {
    std::env::current_exe().ok().and_then(|e| e.parent().map(|p| same_path(p, &install_dir()))).unwrap_or(false)
}

fn powershell(script: &str) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    if status.success() { Ok(()) } else { Err(anyhow!("powershell falhou ({status})")) }
}

fn create_shortcut(target: &Path) -> Result<()> {
    let esc = |p: &Path| p.display().to_string().replace('\'', "''");
    let dir = target.parent().unwrap_or(target);
    powershell(&format!(
        "$s = New-Object -ComObject WScript.Shell; \
         $l = $s.CreateShortcut([IO.Path]::Combine([Environment]::GetFolderPath('Desktop'), 'OP2.lnk')); \
         $l.TargetPath = '{t}'; $l.WorkingDirectory = '{d}'; $l.IconLocation = '{t},0'; \
         $l.Description = 'OP2 - builds do OP.gg no LoL'; $l.Save()",
        t = esc(target),
        d = esc(dir)
    ))
}

/// Copia este .exe para %LOCALAPPDATA%\Programs\OP2, cria o atalho e abre a cópia instalada.
pub fn install_and_relaunch() -> Result<PathBuf> {
    let me = std::env::current_exe()?;
    let dir = install_dir();
    std::fs::create_dir_all(&dir)?;
    let target = dir.join("op2.exe");

    // fecha a versão instalada que estiver aberta, para poder substituir o arquivo
    let _ = powershell(&format!(
        "Get-Process -Name op2 -ErrorAction SilentlyContinue | Where-Object Id -ne {} | Stop-Process -Force",
        std::process::id()
    ));
    let mut copied = Err(anyhow!("não copiou"));
    for _ in 0..10 {
        match std::fs::copy(&me, &target) {
            Ok(_) => {
                copied = Ok(());
                break;
            }
            Err(e) => {
                copied = Err(e.into());
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
    copied?;
    create_shortcut(&target)?;
    Command::new(&target).arg("--wait").current_dir(&dir).spawn()?;
    Ok(dir)
}

// ---------- atualização ----------

#[derive(Clone)]
pub struct Release {
    pub version: String,
    url: String,
}

fn version_parts(s: &str) -> Vec<u64> {
    s.trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("op2-updater/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(90))
        .build()
        .expect("http client")
}

/// Procura no GitHub uma versão mais nova que a atual.
pub async fn check() -> Result<Option<Release>> {
    if REPO.starts_with("OWNER/") {
        return Ok(None);
    }
    let res = client()
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None); // ainda não tem release publicada
    }
    let j: Value = res.error_for_status()?.json().await?;
    let tag = j["tag_name"].as_str().unwrap_or_default();
    if version_parts(tag) <= version_parts(VERSION) {
        return Ok(None);
    }
    let url = j["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case("op2.exe")))
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or_else(|| anyhow!("a release {tag} não tem op2.exe"))?;
    Ok(Some(Release { version: tag.trim_start_matches('v').into(), url: url.into() }))
}

/// Baixa a versão nova, troca o .exe (o Windows deixa renomear um .exe em uso) e abre a nova.
async fn install(r: &Release) -> Result<()> {
    let bytes = client().get(&r.url).send().await?.error_for_status()?.bytes().await?;
    if bytes.len() < 100_000 || !bytes.starts_with(b"MZ") {
        return Err(anyhow!("download inválido"));
    }
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| anyhow!("pasta do app desconhecida"))?;
    let (new, old) = (dir.join("op2.new.exe"), dir.join("op2.old.exe"));
    std::fs::write(&new, &bytes)?;
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old)?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe); // desfaz
        return Err(e.into());
    }
    Command::new(&exe).arg("--wait").current_dir(dir).spawn()?;
    Ok(())
}

/// Remove o .exe antigo deixado pela última atualização.
pub fn cleanup_old() {
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        let _ = std::fs::remove_file(dir.join("op2.old.exe"));
        let _ = std::fs::remove_file(dir.join("op2.new.exe"));
    }
}

/// Tarefa de atualização: ao abrir, atualiza sozinho se tiver versão nova; depois verifica
/// a cada 6h e avisa no menu. Cada mensagem em `clicks` = usuário clicou em "Atualizar".
pub async fn run(notify: Arc<dyn Fn(UiEvent) + Send + Sync>, mut clicks: UnboundedReceiver<()>) {
    cleanup_old();
    let mut latest: Option<Release> = None;

    match check().await {
        Ok(Some(r)) => {
            log(&format!("Nova versão v{} encontrada. Atualizando...", r.version));
            notify(UiEvent::Info(format!("Atualizando para v{}...", r.version)));
            match install(&r).await {
                Ok(()) => return notify(UiEvent::Exit),
                Err(e) => {
                    log(&format!("Falha ao atualizar: {e}"));
                    notify(UiEvent::UpdateAvailable(r.version.clone()));
                    latest = Some(r);
                }
            }
        }
        Ok(None) => {}
        Err(e) => log(&format!("Não foi possível verificar atualizações: {e}")),
    }

    let mut timer = tokio::time::interval(CHECK_EVERY);
    timer.tick().await; // o primeiro tick é imediato; já verificamos acima
    loop {
        tokio::select! {
            _ = timer.tick() => {
                if let Ok(Some(r)) = check().await {
                    log(&format!("Nova versão disponível: v{}", r.version));
                    notify(UiEvent::UpdateAvailable(r.version.clone()));
                    latest = Some(r);
                }
            }
            Some(()) = clicks.recv() => {
                if latest.is_none() {
                    match check().await {
                        Ok(Some(r)) => latest = Some(r),
                        Ok(None) => { notify(UiEvent::Info(format!("Você já está na versão mais recente (v{VERSION})"))); continue; }
                        Err(e) => { notify(UiEvent::Info(format!("Erro ao verificar: {e}"))); continue; }
                    }
                }
                let Some(r) = latest.clone() else { continue };
                notify(UiEvent::Info(format!("Atualizando para v{}...", r.version)));
                match install(&r).await {
                    Ok(()) => return notify(UiEvent::Exit),
                    Err(e) => {
                        log(&format!("Falha ao atualizar: {e}"));
                        notify(UiEvent::Info(format!("Falha ao atualizar: {e}")));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::version_parts;

    #[test]
    fn compares_versions() {
        assert!(version_parts("v0.2.0") > version_parts("0.1.0"));
        assert!(version_parts("v0.10.0") > version_parts("0.9.3"));
        assert!(version_parts("v0.1.0") <= version_parts("0.1.0"));
    }
}
