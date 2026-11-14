// Conexão com a LCU (API local do cliente do LoL): REST + WebSocket.
use anyhow::{anyhow, Result};
use base64::Engine;
use reqwest::Method;
use serde_json::Value;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const LOCKFILE: &str = r"C:\Riot Games\League of Legends\lockfile";

pub type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone)]
pub struct Creds {
    pub port: u16,
    pub password: String,
}

/// Lê porta/senha do lockfile. Se `allow_fallback`, tenta também a linha de comando
/// do LeagueClientUx.exe (para instalações fora da pasta padrão).
pub fn get_credentials(allow_fallback: bool) -> Option<Creds> {
    if let Ok(s) = std::fs::read_to_string(LOCKFILE) {
        let parts: Vec<&str> = s.trim().split(':').collect();
        if parts.len() >= 4 {
            if let Ok(port) = parts[2].parse() {
                return Some(Creds { port, password: parts[3].to_string() });
            }
        }
    }
    if !allow_fallback {
        return None;
    }
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "(Get-CimInstance Win32_Process -Filter \"name='LeagueClientUx.exe'\").CommandLine",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let port = find_arg(&s, "--app-port=")?.parse().ok()?;
    let password = find_arg(&s, "--remoting-auth-token=")?;
    Some(Creds { port, password })
}

fn find_arg(s: &str, key: &str) -> Option<String> {
    let rest = &s[s.find(key)? + key.len()..];
    let end = rest.find(|c: char| c == '"' || c.is_whitespace()).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

pub struct Lcu {
    http: reqwest::Client,
    base: String,
    auth: String,
    port: u16,
}

impl Lcu {
    pub fn new(creds: Creds) -> Result<Self> {
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(true) // certificado autoassinado da Riot
            .danger_accept_invalid_hostnames(true)
            .timeout(Duration::from_secs(10))
            .build()?;
        let auth = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("riot:{}", creds.password))
        );
        Ok(Self { http, base: format!("https://127.0.0.1:{}", creds.port), auth, port: creds.port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{}", self.base, path))
            .header("Authorization", &self.auth)
            .header("Accept", "application/json");
        if let Some(b) = body {
            req = req.json(b);
        }
        let res = req.send().await?;
        let status = res.status();
        let text = res.text().await?;
        let val: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.clone()))
        };
        if !status.is_success() {
            let msg = val.get("message").and_then(Value::as_str).map(String::from).unwrap_or(text);
            return Err(anyhow!("{method} {path}: {} {msg}", status.as_u16()));
        }
        Ok(val)
    }

    pub async fn get(&self, p: &str) -> Result<Value> {
        self.request(Method::GET, p, None).await
    }
    pub async fn post(&self, p: &str, b: &Value) -> Result<Value> {
        self.request(Method::POST, p, Some(b)).await
    }
    pub async fn put(&self, p: &str, b: &Value) -> Result<Value> {
        self.request(Method::PUT, p, Some(b)).await
    }
    pub async fn patch(&self, p: &str, b: &Value) -> Result<Value> {
        self.request(Method::PATCH, p, Some(b)).await
    }
    pub async fn delete(&self, p: &str) -> Result<Value> {
        self.request(Method::DELETE, p, None).await
    }

    pub async fn connect_ws(&self) -> Result<Ws> {
        let tls = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()?;
        let mut req = format!("wss://127.0.0.1:{}/", self.port).into_client_request()?;
        req.headers_mut().insert("Authorization", self.auth.parse()?);
        let (ws, _) = tokio_tungstenite::connect_async_tls_with_config(
            req,
            None,
            false,
            Some(tokio_tungstenite::Connector::NativeTls(tls)),
        )
        .await?;
        Ok(ws)
    }
}
