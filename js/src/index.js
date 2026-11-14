// OP2: espera o cliente do LoL, detecta a seleção de campeão e aplica a build do OP.gg.
//   node src/index.js                      -> modo normal (fica rodando)
//   node src/index.js teste Ahri [mid]     -> aplica runas+itens de um campeão agora (sem champ select)
const fs = require('fs');
const path = require('path');
const { LCU, getCredentials } = require('./lcu');
const { getBuild, getChampions } = require('./opgg');
const { applyRunes, applySpells, applyItems } = require('./apply');

const DEFAULT_CONFIG = { runes: true, spells: true, items: true, flashKey: 'auto' };
const configPath = path.join(__dirname, '..', 'config.json');
let config = { ...DEFAULT_CONFIG };
try { config = { ...config, ...JSON.parse(fs.readFileSync(configPath, 'utf8')) }; } catch {}

const log = (...a) => console.log(new Date().toLocaleTimeString('pt-BR'), ...a);

const POSITION_MAP = { top: 'top', jungle: 'jungle', middle: 'mid', bottom: 'adc', utility: 'support' };

async function detectGameMode(lcu) {
  try {
    const s = await lcu.get('/lol-gameflow/v1/session');
    const mode = s?.gameData?.queue?.gameMode || '';
    const queueId = s?.gameData?.queue?.id;
    if (mode.includes('ARAM') || mode === 'KIWI') return 'aram'; // KIWI = ARAM: Caos
    if (mode.includes('URF')) return 'urf';
    if (mode === 'NEXUSBLITZ') return 'nexus_blitz';
    if (mode === 'CHERRY') return null; // Arena: OP.gg MCP não suporta
    if (queueId === 440) return 'flex';
    return 'ranked';
  } catch { return 'ranked'; }
}

async function applyAll(lcu, championId, gameMode, position, inChampSelect) {
  const build = await getBuild(championId, gameMode, position);
  const wr = build.data.summary?.average_stats?.win_rate;
  log(`→ ${build.champ.name} (${build.gameMode}/${build.position})${wr ? ` winrate ${(wr * 100).toFixed(1)}%` : ''}`);

  const me = await lcu.get('/lol-summoner/v1/current-summoner');
  const tasks = [];
  if (config.runes) tasks.push(['Runas', () => applyRunes(lcu, build)]);
  if (config.spells && inChampSelect) tasks.push(['Feitiços', () => applySpells(lcu, build, config.flashKey)]);
  if (config.items) tasks.push(['Itens', () => applyItems(lcu, build, me.summonerId)]);

  for (const [name, fn] of tasks) {
    try { log(`  ${name}: ${await fn()}`); }
    catch (e) { log(`  ${name}: ERRO ${e.message}`); }
  }
}

function startWatcher(lcu) {
  let lastKey = null;
  let timer = null;

  async function onSession(session) {
    const me = session.myTeam?.find((m) => m.cellId === session.localPlayerCellId);
    const championId = me?.championId;
    if (!championId) return;
    const position = POSITION_MAP[me.assignedPosition] || null;
    const key = `${session.gameId}:${championId}:${position}`;
    if (key === lastKey) return;
    lastKey = key;

    // espera um pouco: evita aplicar várias vezes se o jogador troca rápido de campeão
    clearTimeout(timer);
    timer = setTimeout(async () => {
      const gameMode = await detectGameMode(lcu);
      if (!gameMode) return log('Modo de jogo sem suporte (Arena). Ignorando.');
      applyAll(lcu, championId, gameMode, position, true).catch((e) => log('Erro:', e.message));
    }, 800);
  }

  return {
    'OnJsonApiEvent_lol-champ-select_v1_session': (ev) => {
      if (ev.eventType === 'Delete') { lastKey = null; return; }
      return onSession(ev.data);
    },
    initial: async () => {
      try { await onSession(await lcu.get('/lol-champ-select/v1/session')); } catch {} // não está em champ select
    },
  };
}

async function run() {
  log('OP2 iniciado. Aguardando o cliente do LoL...');
  getChampions().catch(() => {}); // pré-carrega a lista de campeões
  let connected = false;

  const tryConnect = async () => {
    if (connected) return;
    const creds = getCredentials();
    if (!creds) return;
    const lcu = new LCU(creds);
    try { await lcu.get('/lol-summoner/v1/current-summoner'); } catch { return; } // cliente ainda carregando
    connected = true;
    const { initial, ...handlers } = startWatcher(lcu);
    lcu.connectEvents(handlers, () => { connected = false; log('Cliente desconectado. Aguardando...'); });
    log(`Conectado ao cliente (porta ${creds.port}). Escolha um campeão!`);
    initial();
  };

  await tryConnect();
  setInterval(tryConnect, 3000);
}

async function testMode(champName, position) {
  const creds = getCredentials();
  if (!creds) return console.log('Abra o cliente do LoL primeiro.');
  const lcu = new LCU(creds);
  const champs = await getChampions();
  const norm = (s) => s.toLowerCase().replace(/[^a-z]/g, '');
  const champ = [...champs.values()].find((c) => norm(c.name) === norm(champName) || norm(c.key) === norm(champName));
  if (!champ) return console.log('Campeão não encontrado:', champName);
  await applyAll(lcu, champ.champion_id, 'ranked', position || null, false);
}

const [cmd, ...args] = process.argv.slice(2);
if (cmd === 'teste') testMode(args[0], args[1]).catch((e) => console.error(e.message));
else run();
