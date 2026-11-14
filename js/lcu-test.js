// Teste de conexão com a LCU (League Client API). Rode com o cliente do LoL aberto:
//   node lcu-test.js
const fs = require('fs');
const https = require('https');
const { execSync } = require('child_process');

function getCredentials() {
  // 1) lockfile na pasta padrão
  const lockfile = 'C:\\Riot Games\\League of Legends\\lockfile';
  if (fs.existsSync(lockfile)) {
    const [, pid, port, password, protocol] = fs.readFileSync(lockfile, 'utf8').split(':');
    return { port, password, protocol, source: 'lockfile' };
  }
  // 2) fallback: linha de comando do LeagueClientUx.exe
  try {
    const cmd = execSync(
      'powershell -NoProfile -Command "(Get-CimInstance Win32_Process -Filter \\"name=\'LeagueClientUx.exe\'\\").CommandLine"',
      { encoding: 'utf8' }
    );
    const port = cmd.match(/--app-port=(\d+)/)?.[1];
    const password = cmd.match(/--remoting-auth-token=([\w-]+)/)?.[1];
    if (port && password) return { port, password, protocol: 'https', source: 'processo' };
  } catch {}
  return null;
}

function lcu(creds, method, path, body) {
  return new Promise((resolve, reject) => {
    const req = https.request(
      {
        host: '127.0.0.1',
        port: creds.port,
        path,
        method,
        rejectUnauthorized: false, // certificado autoassinado da Riot
        headers: {
          Authorization: 'Basic ' + Buffer.from('riot:' + creds.password).toString('base64'),
          'Content-Type': 'application/json',
          Accept: 'application/json',
        },
      },
      (res) => {
        let data = '';
        res.on('data', (c) => (data += c));
        res.on('end', () => {
          try { resolve({ status: res.statusCode, body: data ? JSON.parse(data) : null }); }
          catch { resolve({ status: res.statusCode, body: data }); }
        });
      }
    );
    req.on('error', reject);
    if (body) req.write(JSON.stringify(body));
    req.end();
  });
}

(async () => {
  const creds = getCredentials();
  if (!creds) {
    console.log('Cliente do LoL não encontrado. Abra o cliente e rode de novo.');
    process.exit(1);
  }
  console.log(`Conectado via ${creds.source} na porta ${creds.port}\n`);

  const me = await lcu(creds, 'GET', '/lol-summoner/v1/current-summoner');
  console.log('Invocador:', me.status, me.body?.gameName ?? me.body?.displayName, '#' + (me.body?.tagLine ?? ''), 'id', me.body?.summonerId);

  const phase = await lcu(creds, 'GET', '/lol-gameflow/v1/gameflow-phase');
  console.log('Fase atual:', phase.body);

  const pages = await lcu(creds, 'GET', '/lol-perks/v1/pages');
  console.log(`\nPáginas de runa (${pages.body?.length ?? 0}):`);
  for (const p of pages.body ?? []) {
    console.log(`  [${p.id}] ${p.name}  editável=${p.isEditable} ativa=${p.current}  primária=${p.primaryStyleId} secundária=${p.subStyleId}`);
  }

  const inv = await lcu(creds, 'GET', '/lol-perks/v1/inventory');
  console.log('\nInventário de páginas:', inv.body);
})().catch((e) => console.error('Erro:', e.message));
