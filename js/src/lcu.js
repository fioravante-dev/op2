// Conexão com a LCU (API local do cliente do LoL): REST + WebSocket.
const fs = require('fs');
const https = require('https');
const { execSync } = require('child_process');
const WebSocket = require('ws');

const LOCKFILE = 'C:\\Riot Games\\League of Legends\\lockfile';

function getCredentials() {
  try {
    if (fs.existsSync(LOCKFILE)) {
      const [, , port, password] = fs.readFileSync(LOCKFILE, 'utf8').split(':');
      if (port && password) return { port, password };
    }
  } catch {}
  // Fallback: pasta não padrão -> lê a linha de comando do LeagueClientUx.exe
  try {
    const cmd = execSync(
      'powershell -NoProfile -Command "(Get-CimInstance Win32_Process -Filter \\"name=\'LeagueClientUx.exe\'\\").CommandLine"',
      { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }
    );
    const port = cmd.match(/--app-port=(\d+)/)?.[1];
    const password = cmd.match(/--remoting-auth-token=([\w-]+)/)?.[1];
    if (port && password) return { port, password };
  } catch {}
  return null;
}

class LCU {
  constructor(creds) {
    this.creds = creds;
    this.auth = 'Basic ' + Buffer.from('riot:' + creds.password).toString('base64');
    this.ws = null;
  }

  request(method, path, body) {
    return new Promise((resolve, reject) => {
      const req = https.request(
        {
          host: '127.0.0.1',
          port: this.creds.port,
          path,
          method,
          rejectUnauthorized: false, // certificado autoassinado da Riot
          headers: { Authorization: this.auth, 'Content-Type': 'application/json', Accept: 'application/json' },
        },
        (res) => {
          let data = '';
          res.on('data', (c) => (data += c));
          res.on('end', () => {
            let parsed = data;
            try { parsed = data ? JSON.parse(data) : null; } catch {}
            if (res.statusCode >= 400) {
              const msg = parsed?.message || data || `HTTP ${res.statusCode}`;
              reject(new Error(`${method} ${path}: ${res.statusCode} ${msg}`));
            } else resolve(parsed);
          });
        }
      );
      req.on('error', reject);
      if (body !== undefined) req.write(JSON.stringify(body));
      req.end();
    });
  }

  get(p) { return this.request('GET', p); }
  post(p, b) { return this.request('POST', p, b); }
  put(p, b) { return this.request('PUT', p, b); }
  patch(p, b) { return this.request('PATCH', p, b); }
  del(p) { return this.request('DELETE', p); }

  // Assina eventos do WebSocket. handlers: { 'OnJsonApiEvent_lol-champ-select_v1_session': fn(event) }
  connectEvents(handlers, onClose) {
    const ws = new WebSocket(`wss://127.0.0.1:${this.creds.port}/`, {
      headers: { Authorization: this.auth },
      rejectUnauthorized: false,
    });
    this.ws = ws;
    ws.on('open', () => {
      for (const ev of Object.keys(handlers)) ws.send(JSON.stringify([5, ev]));
    });
    ws.on('message', (raw) => {
      let msg;
      try { msg = JSON.parse(raw.toString()); } catch { return; }
      // Formato: [8, "NomeDoEvento", { data, eventType, uri }]
      if (Array.isArray(msg) && msg[0] === 8 && handlers[msg[1]]) {
        Promise.resolve(handlers[msg[1]](msg[2])).catch((e) => console.error('Erro no evento:', e.message));
      }
    });
    ws.on('close', () => onClose?.());
    ws.on('error', () => {}); // 'close' vem em seguida
    return ws;
  }
}

module.exports = { LCU, getCredentials };
