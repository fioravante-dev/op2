// Aplica runas, feitiços e sets de itens no cliente.
const PREFIX = 'OP2'; // tudo que o app cria começa com isso; só apagamos o que é nosso

const POS_LABEL = { top: 'Top', jungle: 'Jungle', mid: 'Mid', adc: 'ADC', support: 'Sup' };
const label = (b) => (b.gameMode === 'aram' ? 'ARAM' : POS_LABEL[b.position] || b.position);

async function applyRunes(lcu, build) {
  const r = build.data.runes;
  if (!r?.primary_rune_ids?.length) return 'sem dados de runa';

  const pages = await lcu.get('/lol-perks/v1/pages');
  for (const p of pages) {
    if (p.isDeletable && p.name.startsWith(PREFIX)) await lcu.del(`/lol-perks/v1/pages/${p.id}`);
  }
  const inv = await lcu.get('/lol-perks/v1/inventory');
  if (!inv.canAddCustomPage) return 'sem espaço para nova página de runas (apague uma no cliente)';

  await lcu.post('/lol-perks/v1/pages', {
    name: `${PREFIX} ${build.champ.name} ${label(build)}`,
    primaryStyleId: r.primary_page_id,
    subStyleId: r.secondary_page_id,
    selectedPerkIds: [...r.primary_rune_ids, ...r.secondary_rune_ids, ...r.stat_mod_ids],
    current: true,
  });
  return 'ok';
}

// flashKey: 'auto' (mantém o Flash onde ele já está), 'D' ou 'F'
async function applySpells(lcu, build, flashKey = 'auto') {
  const ids = build.data.summoner_spells?.ids;
  if (!ids || ids.length < 2) return 'sem dados de feitiço';
  let [s1, s2] = ids;
  const FLASH = 4;

  if (ids.includes(FLASH)) {
    let key = flashKey;
    if (key === 'auto') {
      const session = await lcu.get('/lol-champ-select/v1/session');
      const me = session.myTeam.find((m) => m.cellId === session.localPlayerCellId);
      key = me?.spell2Id === FLASH ? 'F' : 'D';
    }
    const other = s1 === FLASH ? s2 : s1;
    [s1, s2] = key === 'F' ? [other, FLASH] : [FLASH, other];
  }
  await lcu.patch('/lol-champ-select/v1/session/my-selection', { spell1Id: s1, spell2Id: s2 });
  return 'ok';
}

function block(title, ids) {
  // agrupa repetidos (ex: 2 poções -> count 2)
  const counts = new Map();
  for (const id of ids) counts.set(String(id), (counts.get(String(id)) || 0) + 1);
  return {
    type: title,
    items: [...counts].map(([id, count]) => ({ id, count })),
    hideIfSummonerSpell: '',
    showIfSummonerSpell: '',
  };
}

const pct = (x) => (x == null ? '' : ` ${(x * 100).toFixed(1)}%`);

async function applyItems(lcu, build, summonerId) {
  const d = build.data;
  const skillOrder = d.skill_masteries?.ids?.join(' > ');
  const firstSkills = d.skills?.order?.slice(0, 6).join(' ');

  const blocks = [];
  if (d.starter_items?.ids) {
    blocks.push(block(`Iniciais${skillOrder ? ` | Skills: ${skillOrder}` : ''}`, d.starter_items.ids));
  }
  if (d.boots?.ids) blocks.push(block(`Botas${firstSkills ? ` | Início: ${firstSkills}` : ''}`, d.boots.ids));
  if (d.core_items?.ids) blocks.push(block(`Core | Win${pct(d.core_items.play ? d.core_items.win / d.core_items.play : null)}`, d.core_items.ids));
  for (const [key, name] of [['fourth_items', '4º item'], ['fifth_items', '5º item'], ['sixth_items', '6º item']]) {
    const opts = d[key];
    if (opts?.length) blocks.push(block(`${name} (opções)`, opts.slice(0, 4).flatMap((o) => o.ids)));
  }
  if (!blocks.length) return 'sem dados de item';

  const current = await lcu.get(`/lol-item-sets/v1/item-sets/${summonerId}/sets`);
  const itemSets = (current.itemSets || []).filter(
    (s) => !(s.title?.startsWith(PREFIX) && s.associatedChampions?.includes(build.champ.champion_id))
  );
  itemSets.unshift({
    title: `${PREFIX} ${build.champ.name} ${label(build)}`,
    type: 'custom',
    map: 'any',
    mode: 'any',
    sortrank: 0,
    startedFrom: 'blank',
    associatedChampions: [build.champ.champion_id],
    associatedMaps: [],
    preferredItemSlots: [],
    uid: crypto.randomUUID(),
    blocks,
  });
  await lcu.put(`/lol-item-sets/v1/item-sets/${summonerId}/sets`, {
    accountId: current.accountId,
    itemSets,
    timestamp: Date.now(),
  });
  return 'ok';
}

module.exports = { applyRunes, applySpells, applyItems };
