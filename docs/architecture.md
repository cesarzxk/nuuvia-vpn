# Arquitetura do Nuuvia

Este documento registra as decisões de projeto para não perdermos o contexto
entre os marcos.

## 1. Objetivo

Permitir que dois ou mais computadores joguem como se estivessem na mesma LAN:

- descoberta de sala do jogo funcionando (broadcast e multicast);
- tráfego do jogo direto entre os peers, com baixa latência;
- criptografia ponta a ponta;
- Linux e Windows;
- LAN primeiro, internet depois.

Não é um desktop remoto. O problema é encaminhar quadros Ethernet, não vídeo.

## 2. Por que Layer 2

Protocolos de descoberta de jogos usam:

- UDP broadcast em `255.255.255.255` (Terraria, OpenTTD, DirectPlay, etc.);
- multicast, muitas vezes com **TTL 1** (Minecraft Java: `224.0.2.60:4445`).

Um VPN L3 (WireGuard, Tailscale, Nebula) entrega IP roteado e descarta
broadcast; multicast com TTL 1 morre no primeiro roteador. A solução é um
**switch Ethernet virtual**: TAP nos dois lados e flooding de broadcast,
multicast e unicast desconhecido.

VPNs L2 comerciais são L2 pelo mesmo motivo.

## 3. Componentes

```
┌───────────────────────────────┐
│ nuvia-gui (egui/eframe)       │  janela + menu REDE/AJUDA
│  várias redes, status, ajustes│
└──────────────┬────────────────┘
               │ chamadas no processo
┌──────────────▼────────────────┐
│ nuvia-core (root/admin)       │
│  TAP compartilhado, rotas, UDP│
│  Noise, AEAD, rendezvous      │
└──────────────┬────────────────┘
               │ UDP cifrado
┌──────────────▼────────────────┐
│ peer / relay                  │
└───────────────────────────────┘
```

Hoje a GUI e o núcleo rodam no mesmo processo (que precisa de root/admin). O
serviço separado e a UI sem privilégios continuam no roadmap (M6); o core já
está separado em `nuvia-core`.

Crates:

| Crate | Papel |
|---|---|
| `nuvia-core` | identidade, sala, Noise, framing, sessão, túnel, rotas |
| `nuvia-cli` | binário `nuvia` |
| `nuvia-gui` | GUI egui/eframe multi-rede |
| `nuvia-protocol` | HELLO/ACK, envelope de relay, convite legado |
| `nuvia-server` | rendezvous + relay |
| `nuvia-probe` | `lanprobe` para validar broadcast/multicast |

## 4. Plano de dados

### 4.1 TAP

- Linux: `/dev/net/tun` com `IFF_TAP`, via `tun-rs`.
- Windows: driver assinado `tap-windows6`, via o mesmo `tun-rs`.
- MTU **1360** (cabe o envelope do relay sem fragmentar; ver 8.5).
- O IP virtual é atribuído no adaptador. O jogo o enxerga como uma Ethernet
  normal.

**Detalhe crítico:** o MAC virtual de cada peer é o **MAC real do seu adaptador
TAP**. Ao injetar um quadro no TAP, o stack do SO só aceita se o MAC de destino
for o MAC do adaptador (ou broadcast/multicast). Se usarmos um MAC inventado, o
frame é descartado antes de chegar ao jogo.

### 4.2 Envelope UDP

Pacote de dados:

```
"NUV1" | tipo=2 | session_id (u64 BE) | counter (u64 BE) | ciphertext + tag
```

- AAD = cabeçalho completo (magic, tipo, sessão, contador).
- Nonce do ChaCha20-Poly1305 = 4 bytes zero + contador BE (12 bytes).
- `session_id` = primeiros 8 bytes do handshake hash.
- Janela de replay de 64 pacotes; UDP fora de ordem é aceito uma única vez.
- Handshake usa `"NUV1" | tipo=1 | mensagem Noise`.

### 4.3 Handshake

Noise **XX** (`Noise_XX_25519_ChaChaPoly_BLAKE2s`):

- autentica as chaves estáticas dos dois lados sem chave prévia;
- o payload cifrado da mensagem 2 leva a semente do responder e o da mensagem 3
  leva a semente do iniciador;
- as chaves de tráfego são derivadas com BLAKE3:

```
ikm  = handshake_hash || seed_iniciador || seed_responder || psk_da_sala
i2r  = BLAKE3-derive-key("nuvia v1 traffic i2r", ikm)
r2i  = BLAKE3-derive-key("nuvia v1 traffic r2i", ikm)
```

Misturar a PSK aqui significa que conhecer a chave pública do peer não basta:
sem a senha da sala, as chaves não batem e os pacotes são rejeitados.

- A chave pública remota é comparada com a configurada. Divergência aborta.
- O papel (iniciador/responder) é determinístico: a menor chave pública inicia.
- Depois do handshake, cada lado envia keepalives autenticados até receber um
  pacote válido do outro; isso detecta a perda da última mensagem.
- Keepalive a cada 10 s; sessão derrubada após 30 s sem pacotes. O laço externo
  refaz o handshake automaticamente.

### 4.4 Identidade e sala

- Identidade = X25519 de longa duração, salva em
  `~/.config/nuvia/identity.toml` (0600).
- `room_id` = BLAKE3("nuvia room id v1" || nome)[..16] (público).
- PSK = Argon2id(senha, salt=room_id, m=19456 KiB, t=2, p=1, 32 bytes).
- A senha nunca é transmitida.
- Token do relay = BLAKE3("nuvia v1 relay token" || PSK)[..16]. Permite entrar
  sem convite; o servidor só guarda o hash do token.
- Octeto da sub-rede = `room_id[0] % 250 + 1`, resultando em
  `10.240.<octeto>.0/24`. Duas salas diferentes raramente colidem; se
  colidirem, use outro nome de sala.

### 4.5 Múltiplas redes na GUI

A GUI não usa VLAN nem um switch multi-rede. Cada sala é uma sessão
independente:

| Recurso | Por sala |
|---|---|
| TAP | um único `nuvia-vpn0` / `nuvia0` para todas as salas |
| Porta UDP | `42424 + índice`, uma por sala |
| Sessão mesh | uma tarefa Tokio por sala, relay em grupo |
| IP virtual | um único `10.240.<hash>.<hash>/16` por máquina |
| Token do relay | derivado do nome/senha daquela sala |

A GUI usa o **modo mesh**: um único TAP e um único IP para todas as salas,
com uma tarefa/socket por sala. Os quadros do TAP local são cifrados com a
chave de cada sala e enviados aos endpoints diretos dos membros e também pelo
relay (duplicatas são descartadas pela janela de replay). O registro no
servidor inclui os IPs locais de cada peer, além do endpoint público observado
pelo servidor; o mesh envia para todos os candidatos, então máquinas na mesma
LAN funcionam mesmo sem NAT hairpin. Cada participante
recebe um ponto de status na lista expandida: verde = pacotes diretos, roxo =
somente relay, cinza = offline/aguardando. O badge do cartão mostra `DIRETO`
só quando todos os participantes ativos estão diretos; se algum depende do
relay, mostra `RELAY` após 8 s. Os quadros recebidos
de qualquer sala são escritos no mesmo TAP, então o Minecraft vê as LANs de
todas as salas ativas. O IP é derivado de `BLAKE3(public_key)` e
vive em `10.240.0.0/16`. O botão ON/OFF da barra conecta/desconecta a sala
selecionada. Com o **botão direito** no cartão abre-se um menu: DESATIVAR
desconecta a sala sem removê-la; SAIR apaga a sala da lista. Um clique no cartão
expande, abaixo da linha, a lista de participantes daquela sala.

O TAP compartilhado é, na prática, um hub L2 local: todas as salas ativas
dividem o mesmo domínio de broadcast. A separação entre salas existe no relay
(chaves de grupo), não no IP local.

O CLI mantém o modo 1:1 com Noise XX e caminho AUTO direto/relay; ele não
interopera com o mesh da GUI.

A lista de redes é salva em `rooms.toml` (0600) ao lado da identidade e
restaurada sem auto-conectar. Fechar a janela esconde para a bandeja quando
há suporte a StatusNotifier; caso contrário encerra o processo. Sair pela
bandeja ou por AJUDA → Sair encerra de verdade.

## 5. Descoberta e mesh (M2)

### 5.1 Beacon

Na interface física, UDP `42424` (broadcast + multicast), a cada 2 s:

```
"NUVB" | versão | room_id | peer_id | pubkey | endpoint | ip_virtual | mac_tap | nome | HMAC(psk, ...)
```

- HMAC prova que quem anuncia conhece a senha, sem revelá-la.
- Peer com mesmo `room_id` e HMAC válido inicia o handshake.
- Timeout de 15 s remove o peer da lista.
- Sem servidor na LAN.

### 5.2 Endereçamento

- Sub-rede `10.240.<hash(sala)>.0/24`, com `--subnet` para conflito.
- No CLI 1:1: menor `public_key` recebe `.1`, maior recebe `.2` na sub-rede da
  sala.
- No mesh da GUI: um único IP `10.240.<a>.<b>/16`, estável por identidade e
  compartilhado por todas as salas; os octetos vêm de BLAKE3(public_key).
- No mesh da GUI, o IP é o mesmo para todas as salas e estável por identidade.
- MAC = MAC do TAP local (ver 4.1). O MAC é anunciado no handshake/beacon.

### 5.3 Switch L2

- Aprende `MAC de origem → peer`.
- Unicast conhecido → só o dono.
- Broadcast, multicast e unicast desconhecido → flood para todos.
- **Nunca** re-floodar um quadro recebido de outro peer (evita loop/tempestade).
- Rate limit de broadcast por peer.
- IPv6 desligado no adaptador na v1 para reduzir ruído de MLD/ND.

## 6. Descoberta de jogos (M3)

Inundar multicast no túnel não basta: o jogo precisa **entrar no grupo multicast
pelo adaptador virtual**. Se o Java fizer `joinGroup` na interface física, os
quadros que chegam pelo TAP são descartados.

Solução comum em VPNs L2:

```bash
# Linux
ip route add 224.0.0.0/4 dev nuvia-vpn0
ip route add 224.0.2.60/32 dev nuvia-vpn0   # Minecraft Java
ip route add 255.255.255.255/32 dev nuvia-vpn0

# Windows (admin)
New-NetRoute -DestinationPrefix 224.0.0.0/4 -InterfaceIndex <nuvia>
New-NetRoute -DestinationPrefix 224.0.2.60/32 -InterfaceIndex <nuvia>
New-NetRoute -DestinationPrefix 255.255.255.255/32 -InterfaceIndex <nuvia>
```

- Verificar com `ip route get 224.0.2.60` / `Find-NetRoute`.
- **Abrir o jogo depois do Nuuvia**; se já estava aberto, reiniciar.
- Exceções para mDNS (`224.0.0.251/32`) e SSDP (`239.255.255.250/32`) pela
  interface física mantêm Chromecast/impressoras funcionando.
- Firewall precisa permitir entrada no TAP (Linux: `ufw allow in on
  nuvia-vpn0`; Windows: perfil Private + regra `nuvia-nuvia0` por adaptador) e o
  UDP do túnel (Linux: `ufw allow 42424/udp`; Windows: a regra `nuvia-app` é
  criada pelo cliente).
- Com várias redes na GUI, **só a rede ativa** recebe essas rotas. Ativar outra
  rede move as rotas; desconectar a ativa libera a descoberta local.

Status: **M3 validado em 16/11/2026** com Minecraft Java 1.8.5, Windows
hospedando e Linux entrando pelo IP virtual. O anúncio
`10.240.0.2.55625 > 224.0.2.60.4445` foi capturado no `nuvia-vpn0`. No Windows,
além das rotas, é necessário `Set-NetIPInterface -InterfaceAlias nuvia-vpn0
-InterfaceMetric 1`; sem isso a Ethernet física ganha a escolha do multicast.

Aceite: mundo LAN do Minecraft Java aparece no outro PC em até 3 s e é jogável,
nas duas direções (Linux hospeda/Windows entra e vice-versa).

## 7. Windows (M1)

- Driver `tap-windows6` assinado (9.27.0), instalado pelo setup uma vez.
- O Nuuvia cria **um único adaptador `nuvia0`** para todas as salas; exigir
  admin enquanto GUI e core rodam no mesmo processo.
- Serviço `LocalSystem` + UI sem privilégios via named pipe/localhost continuam
  no roadmap (M6).
- Perfil de rede **Private** + regra de firewall de entrada `nuvia-nuvia0`;
  sem isso a descoberta não funciona.
- Métrica da interface do TAP em **1**; sem isso o Windows escolhe a Ethernet
  física para multicast.
- IP/MTU via IP Helper ou `netsh`.
- `nuvia doctor` valida driver, adaptador, IP, rotas, firewall e perfil.
- Cross-compile: `x86_64-pc-windows-gnu` com mingw-w64. Para o servidor ARM
  (Oracle Ampere), build nativo na VPS ou `cross`.

## 8. Internet e relay (M5)

### 8.1 Caminhos

```
1. LAN              A ─────────────── B
2. Internet direta  A ── hole punch ── B
3. Relay            A ───► servidor ───► B
```

Hoje o CLI **AUTO** tenta o hole punching direto por 8 s e cai no relay
sozinho; o CLI pode forçar `--relay` ou `--direct`. A GUI usa mesh e envia para
os endpoints diretos e para o relay em paralelo, mostrando o caminho por peer.
A migração de volta para direct e o fallback automático após queda ficam para o
M6.

### 8.2 Rendezvous

Implementado no M5a (crates `nuvia-protocol`, `nuvia-server` e
`nuvia-core::rendezvous`):

- `POST /v1/register` com `room_id`, `peer_id`, `token` e `name`. A sala é
  criada no primeiro registro e autenticada pelo hash do token.
- UDP `8443`: o cliente envia `HELLO`; o servidor grava o endpoint público
  observado e responde `ACK`. O cliente registra de novo e recebe os peers.
- Estado em memória: peer expira em 60 s sem contato; sala vazia dura 24 h.
- O token padrão é derivado de `BLAKE3(PSK)[..16]`, então a GUI não precisa de
  convite. O servidor guarda apenas o hash do token. O `nuvia invite` com token
  aleatório continua como compatibilidade. Uma sala criada com convite fica
  presa a esse token até o servidor reiniciar; nesse caso use outro nome ou
  reinicie o `nuvia-server` (estado em memória).
- Sem contas. Rate limit por IP/sala ainda é um TODO de produção.
- O hole punching é feito pelos próprios clientes: o responder envia pacotes
  `TYPE_PUNCH` enquanto espera a mensagem 1 do Noise.

Teste local: `scripts/test-m5.sh` (servidor + bridge + dois namespaces + ping).

### 8.3 Relay

Implementado no M5b (servidor e cliente). Envelope externo:

```
"NUVR" | ver (2) | room_id (16) | src_tag (8) | dst_tag (8) | token (16) | inner
```

- `src_tag`/`dst_tag` = primeiros 8 bytes da chave pública de cada peer.
- `inner` é o mesmo pacote do túnel (`NUV1` handshake/dados/punch), já cifrado.
- O servidor valida sala/token, atualiza o endpoint do remetente e encaminha
  **apenas o `inner`** para o endpoint do destino. Ele não descriptografa e não
  entende Ethernet/IP.
- O cliente só aceita pacotes vindos do IP:porta do servidor; o AEAD do túnel
  rejeita qualquer injeção.
- Broadcast/multicast: o cliente mesh envia um envelope com `dst_tag = 0`; o
  servidor reenvia o mesmo `inner` para todos os outros peers. O custo cresce
  com o número de participantes. O padrão da GUI é 8; o servidor aceita salas
  customizadas de até 32 (`--max-peers` no CLI).
- Keepalive de dados a cada 10 s; o relay também atualiza o `last_seen`, então o
  TTL de 60 s não derruba uma sessão ativa.
- Ainda **não há cap de banda por sala**; o servidor de 1 GB atende o uso atual.
- Teste: `NUVIA_TEST_RELAY=1 ./scripts/test-m5.sh`.

### 8.4 Servidor

- Binário Rust `nuvia-server` (rendezvous + relay + endpoint discovery).
- Caddy na frente para TLS (Let's Encrypt); API em `127.0.0.1:4646`.
- UDP `8443` para relay/descoberta; TCP `443` para API; TCP `80` para ACME.
- systemd, usuário `nuvia`, hardening, `/healthz` e `/metrics` no loopback.
- Estado em memória; reinício derruba sessões e os clientes reconectam.
- Deploy Oracle: reserved public IP, security list TCP 22/80/443 + UDP 8443,
  firewall do guest (firewalld/iptables + remoção do REJECT da imagem Ubuntu),
  binário estático musl enviado pronto.
- Instância atual: `167.234.237.6`, `https://167.234.237.6.sslip.io`, região
  `sa-saopaulo-1`, Ubuntu 20.04 x86_64. Caddy emite Let's Encrypt de produção;
  systemd mantém `nuvia-server` e `caddy`.

### 8.5 MTU

Relay adiciona 54 bytes de envelope. MTU 1360 no TAP evita fragmentação; se o
relay for IPv6, 1340. Sem fragmentação no túnel na v1.

### 8.6 Custo

Minecraft usa ~0,5–2 Mbps. 8 h ≈ 2–7 GB. O Always Free da Oracle inclui 10 TB/mês
de egress; um VPS de US$ 5–6 atende bem. O gargalo é banda, não CPU.

## 9. Segurança

- E2E: o relay vê apenas ciphertext, tamanhos e horários.
- Mesh: chave de grupo derivada da PSK (`BLAKE3("nuvia v1 group data")`), com
  XChaCha20-Poly1305 e época aleatória por processo. Autentica a sala, não um
  peer individual: qualquer membro com a senha pode injetar quadros L2.
- Noise XX + verificação da chave estática + PSK no modo 1:1 do CLI.
- Mesh da GUI: XChaCha20-Poly1305 com chave de grupo, época aleatória e janela
  de replay por remetente.
- AEAD com AAD, contador por direção e janela de replay.
- Identidade 0600; `.gitignore` bloqueia chaves e `.env`.
- O token do relay é derivado da PSK para evitar convite. Quem controla o
  servidor pode tentar adivinhar a senha offline (Argon2id por tentativa); use
  uma senha longa. O tráfego continua protegido pela PSK.
- Chave SSH da VPS fora do repositório (`~/.ssh/nuvia/`).
- Servidor self-hosted primeiro; instância pública exigiria rate limit e
  invite-only para não virar proxy de abuso.

## 10. Aceite por marco

| Marco | Critério |
|---|---|
| M0 | `ping 10.240.x.y` e broadcast direcionado entre dois netns ✅ |
| M1a | Windows: TAP, rotas, firewall, métrica e ping ✅ |
| M1b | GUI compacta multi-rede, lista persistida e tray ✅ |
| M2 | 2–8 peers se descobrem sozinhos; status por peer e TAP/IP compartilhado ✅ |
| M3a | rotas de descoberta + multicast `224.0.2.60:4445` no teste ✅ |
| M3 | Minecraft Java 1.8.5 LAN Linux ↔ Windows ✅ |
| M4 | instaladores NSIS/.deb/AppImage, ícones e tray ✅; assinatura/autostart no M6 |
| M5a | rendezvous + convite + handshake direto (teste local) ✅ |
| M5b | relay cifrado + NAT simétrico (teste local e VPS) ✅ |
| M5 | Minecraft entre duas redes diferentes via relay ✅ |
