// Busca builds no servidor MCP público do OP.gg (https://mcp-api.op.gg/mcp).
const MCP_URL = 'https://mcp-api.op.gg/mcp';
let rpcId = 0;

async function callTool(name, args) {
  const res = await fetch(MCP_URL, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream' },
    body: JSON.stringify({ jsonrpc: '2.0', id: ++rpcId, method: 'tools/call', params: { name, arguments: args } }),
    signal: AbortSignal.timeout(20000),
  });
  let text = await res.text();
  // O servidor pode responder como SSE ("data: {...}")
  if (/^(event|data):/m.test(text) && !text.trimStart().startsWith('{')) {
    text = text.split('\n').filter((l) => l.startsWith('data:')).map((l) => l.slice(5)).join('');
  }
  const json = JSON.parse(text);
  if (json.error) throw new Error(json.error.message);
  const out = json.result?.content?.[0]?.text ?? '';
  if (json.result?.isError) throw new Error(out);
  return parseCompact(out);
}

// O OP.gg responde num formato compacto:
//   class Data: core_items,runes
//   class Runes: primary_page_id,primary_rune_ids
//   Root(Data(CoreItems([3118],["Malignance"]),Runes(8100,[8112])))
// Esta função transforma isso em objetos JS usando os nomes de campo do cabeçalho.
function parseCompact(text) {
  const lines = text.split('\n');
  const classes = {};
  let i = 0;
  for (; i < lines.length; i++) {
    const m = lines[i].match(/^class (\w+): (.*)$/);
    if (!m) break;
    classes[m[1]] = m[2].split(',');
  }
  const src = lines.slice(i).join('\n').trim();
  let p = 0;
  const NUM = /-?\d+(?:\.\d+)?(?:[eE][-+]?\d+)?/y;
  const IDENT = /\w+/y;
  const skipWs = () => { while (p < src.length && /\s/.test(src[p])) p++; };
  const fail = () => { throw new Error(`Resposta do OP.gg inválida perto de: ${src.slice(p, p + 40)}`); };

  function list(close) {
    const out = [];
    skipWs();
    if (src[p] === close) { p++; return out; }
    for (;;) {
      out.push(value());
      skipWs();
      if (src[p] === ',') { p++; continue; }
      if (src[p] === close) { p++; return out; }
      fail();
    }
  }

  function value() {
    skipWs();
    const c = src[p];
    if (c === '[') { p++; return list(']'); }
    if (c === '"') {
      let e = p + 1;
      while (e < src.length && src[e] !== '"') { if (src[e] === '\\') e++; e++; }
      const s = JSON.parse(src.slice(p, e + 1));
      p = e + 1;
      return s;
    }
    NUM.lastIndex = p;
    const n = NUM.exec(src);
    if (n) { p = NUM.lastIndex; return Number(n[0]); }
    IDENT.lastIndex = p;
    const id = IDENT.exec(src);
    if (!id) fail();
    p = IDENT.lastIndex;
    skipWs();
    if (src[p] === '(') {
      p++;
      const args = list(')');
      const fields = classes[id[0]] || [];
      const obj = {};
      args.forEach((a, k) => (obj[fields[k] ?? k] = a));
      return obj;
    }
    const lit = { null: null, None: null, true: true, True: true, false: false, False: false };
    return id[0] in lit ? lit[id[0]] : id[0];
  }

  return value();
}

// ---- Campeões ----
let championsCache = null;
async function getChampions() {
  if (championsCache) return championsCache;
  const r = await callTool('lol_list_champions', {
    desired_output_fields: ['data.champions[].{champion_id,key,name}'],
  });
  championsCache = new Map(r.data.champions.map((c) => [c.champion_id, c]));
  return championsCache;
}

// "TwistedFate" -> "TWISTED_FATE"
function opggNames(champ) {
  const fromKey = champ.key.replace(/([a-z])([A-Z])/g, '$1_$2').toUpperCase();
  const fromName = champ.name.replace(/[^A-Za-z ]/g, '').trim().replace(/\s+/g, '_').toUpperCase();
  return [...new Set([fromKey, fromName])];
}

const BUILD_FIELDS = [
  'data.runes.{primary_page_id,primary_rune_ids[],secondary_page_id,secondary_rune_ids[],stat_mod_ids[],pick_rate,win,play}',
  'data.summoner_spells.{ids[],pick_rate,win,play}',
  'data.starter_items.{ids[],pick_rate,win,play}',
  'data.boots.{ids[],pick_rate,win,play}',
  'data.core_items.{ids[],pick_rate,win,play}',
  'data.fourth_items[].{ids[],pick_rate}',
  'data.fifth_items[].{ids[],pick_rate}',
  'data.sixth_items[].{ids[],pick_rate}',
  'data.skills.{order[]}',
  'data.skill_masteries.{ids[]}',
  'data.summary.average_stats.{win_rate,tier}',
];

async function analysis(champ, gameMode, position, fields) {
  let lastErr;
  for (const name of opggNames(champ)) {
    try {
      const r = await callTool('lol_get_champion_analysis', {
        game_mode: gameMode, champion: name, position, desired_output_fields: fields,
      });
      return r.data;
    } catch (e) { lastErr = e; }
  }
  throw lastErr;
}

// Rota principal do campeão (usada quando o cliente não informa a rota, ex: normal blind)
async function mainPosition(champ) {
  const d = await analysis(champ, 'ranked', 'mid', ['data.summary.positions[].name']);
  const name = d?.summary?.positions?.[0]?.name?.toLowerCase();
  return name || 'mid';
}

async function getBuild(championId, gameMode, position) {
  const champ = (await getChampions()).get(championId);
  if (!champ) throw new Error(`Campeão ${championId} não encontrado no OP.gg`);
  if (!position) position = gameMode === 'aram' ? 'mid' : await mainPosition(champ);
  const data = await analysis(champ, gameMode, position, BUILD_FIELDS);
  return { champ, position, gameMode, data };
}

module.exports = { getBuild, getChampions, parseCompact };
