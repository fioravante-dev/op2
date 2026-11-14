# OP2

App levíssimo para League of Legends: quando você escolhe um campeão, aplica
automaticamente **runas, feitiços e set de itens** com as builds do OP.gg.
Sem propaganda, sem janela: só um ícone na bandeja.

- `.exe` único de ~1,5 MB, ~5 MB de RAM
- Detecta rota e modo (ranked, flex, ARAM, ARAM: Caos, URF)
- Mantém o Flash na tecla que você já usa
- Ao sair patch novo, limpa os sets de itens antigos
- Se atualiza sozinho pelas Releases do GitHub

## Instalar

1. Baixe o `op2.exe` da [última release](../../releases/latest).
2. Dê dois cliques. Ele se instala em `%LOCALAPPDATA%\Programs\OP2`,
   cria o atalho **OP2** na área de trabalho e já fica rodando na bandeja.

Para desinstalar: feche pelo menu da bandeja (Sair), apague a pasta
`%LOCALAPPDATA%\Programs\OP2`, o atalho e (opcional) `%APPDATA%\op2`.

## Menu da bandeja

| Opção | O que faz |
|---|---|
| Reaplicar build agora | Força a aplicação para o campeão atual (se algo bugar) |
| Reaplicar como... | Reaplica escolhendo a rota (útil em blind pick) |
| Runas / Feitiços / Itens | Liga ou desliga cada parte |
| Flash | Manter onde está, sempre no D ou sempre no F |
| Verificar atualizações | Procura versão nova no GitHub |
| Abrir log | Mostra o que o app fez (`%APPDATA%\op2\op2.log`) |

## Como funciona

- **Cliente do LoL:** API local do cliente (LCU), usando a porta e senha do
  arquivo `lockfile` e o WebSocket para saber quando você escolhe um campeão.
- **Builds:** servidor MCP público do OP.gg (`https://mcp-api.op.gg/mcp`).
- Tudo que o app cria começa com **"OP2"**; suas páginas e sets ficam intactos.

## Desenvolvimento

```powershell
cargo build                    # debug (com console)
cargo test
.\target\debug\op2.exe teste Ahri mid   # aplica runas+itens agora, sem champ select
powershell -ExecutionPolicy Bypass -File install.ps1   # compila e instala local
```

### Publicar versão nova

1. Mude `version` no `Cargo.toml` (ex: `0.2.0`) e faça commit.
2. `git tag v0.2.0` e `git push origin main --tags`
3. O GitHub Actions compila e publica o `op2.exe`. Os apps instalados se
   atualizam sozinhos na próxima vez que abrirem.

A pasta `js/` tem o protótipo original em Node.js (referência).
