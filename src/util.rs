// Pasta de dados, log e configuração (%APPDATA%\op2).
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    let dir = PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("op2");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn log_path() -> PathBuf {
    data_dir().join("op2.log")
}

pub fn log(msg: &str) {
    let line = format!("{} {}\n", chrono::Local::now().format("%d/%m %H:%M:%S"), msg);
    print!("{line}");
    let path = log_path();
    if fs::metadata(&path).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = fs::remove_file(&path);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub runes: bool,
    pub spells: bool,
    pub items: bool,
    /// "auto" (mantém o Flash onde já está), "D" ou "F"
    #[serde(rename = "flashKey")]
    pub flash_key: String,
    /// Último patch do LoL visto (ex: "16.22"); quando muda, limpa os sets antigos.
    #[serde(rename = "lastPatch")]
    pub last_patch: String,
}

impl Default for Config {
    fn default() -> Self {
        Self { runes: true, spells: true, items: true, flash_key: "auto".into(), last_patch: String::new() }
    }
}

impl Config {
    fn path() -> PathBuf {
        data_dir().join("config.json")
    }
    pub fn load() -> Self {
        let Ok(text) = fs::read_to_string(Self::path()) else { return Self::default() };
        // Bloco de Notas/PowerShell podem salvar com BOM, que o serde_json não aceita
        match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
            Ok(c) => c,
            Err(e) => {
                log(&format!("config.json inválido ({e}); usando padrões"));
                Self::default()
            }
        }
    }
    pub fn save(&self) {
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = fs::write(Self::path(), s);
        }
    }
}

/// Garante uma única instância (atalho clicado duas vezes). Retorna false se já existe outra.
/// Com `wait`, espera até ~10s a outra fechar (usado logo após instalar/atualizar).
pub fn single_instance(wait: bool) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\OP2-single-instance\0".encode_utf16().collect();
    for _ in 0..if wait { 40 } else { 1 } {
        // O handle fica aberto até o processo terminar (de propósito).
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if !handle.is_null() && !exists {
            return true;
        }
        if !handle.is_null() {
            unsafe { CloseHandle(handle) };
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    false
}

pub fn message_box(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    unsafe { MessageBoxW(std::ptr::null_mut(), w(text).as_ptr(), w("OP2").as_ptr(), MB_OK | MB_ICONINFORMATION) };
}
