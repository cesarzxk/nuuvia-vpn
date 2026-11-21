# Windows

O cliente Windows usa o driver **tap-windows6** (o mesmo driver do OpenVPN)
para criar uma Ethernet virtual. O driver é assinado e funciona no Windows 10 e
11. Ele é GPLv2 e é distribuído junto no instalador, não é linkado ao Nuuvia.

A GUI usa **OpenGL (glow)** por padrão, que funciona em placas antigas, e cai
automaticamente para **wgpu** (DirectX 12/Vulkan) quando o OpenGL não está
disponível. Se os dois falharem (por exemplo, OpenGL 1.1 e nenhum adaptador
DX12/Vulkan), o app reinicia sozinho a cópia em `soft\`, que usa **Mesa3D
llvmpipe** — OpenGL por software, sem precisar de GPU ou driver de vídeo. Para
forçar um renderer, defina `NUVIA_RENDERER=wgpu` ou `NUVIA_RENDERER=glow` antes
de abrir o app.

## Instalador (recomendado)

Baixe `nuvia-setup-x86_64.exe` e execute como administrador. O instalador:

- copia `nuvia-gui.exe`, `nuvia.exe` e `lanprobe.exe` para
  `%ProgramFiles%\nuvia`;
- inclui e instala o driver assinado `tap-windows6` 9.27.0 (amd64);
- cria atalhos na área de trabalho e no menu Iniciar;
- registra o desinstalador em **Aplicativos e recursos**.

O Windows vai pedir UAC. Se o driver já estiver instalado, o instalador segue
sem reinstalar; se o adaptador `nuvia0` não existir, o próprio app tenta
reparar/renomear o TAP ao conectar.

## GUI

O `nuvia-gui.exe` tem tema dark/neon e manifesto `requireAdministrator` (o
Windows pede UAC ao abrir). A janela é compacta e não pede servidor, caminho,
TAP, porta, papel nem IP.

1. Abra `nuvia-gui.exe` e aceite o UAC.
2. No primeiro PC: menu **REDE** → **CRIAR REDE**.
3. No segundo PC: menu **REDE** → **ENTRAR EM REDE** com o **mesmo nome** e a
   **mesma senha**. Entrar só funciona se a sala já existir; a GUI não cria uma
   sala nova silenciosamente quando o nome ou a senha estão errados.
4. Abra o jogo **depois** do Nuuvia e use **Open to LAN**.

Cada rede tem **FIREWALL** no menu de contexto e um selo (`FW GLOBAL`,
`FW GAMES`, `FW BLOCK`, `FW CUSTOM`). O filtro controla TCP/UDP/ICMP IPv4 entre
o PC e os peers daquela sala; ARP é sempre permitido. O padrão global fica em
**REDE → FIREWALL** e cada rede pode sobrescrevê-lo. Mudanças
aplicam imediatamente às redes conectadas. No modo **PERSONALIZADO**, o
botão **APPS** importa as portas TCP/UDP dos aplicativos em execução e vira
**REMOVE** para retirá-las. Em **Criar rede**, a senha é opcional e **Rede
pública** lista a sala no diretório; **Entrar em rede** mostra as redes
públicas com os selos ABERTA/COM SENHA. Sem senha, a chave é derivada do nome,
então qualquer pessoa que souber o nome pode entrar e ler o tráfego. O firewall
roda no mesh da GUI; o modo pairwise do CLI não usa esse filtro.

O instalador registra o serviço privilegiado `nuvia-helper`; a GUI roda como
usuário normal e usa o helper para criar o TAP e as rotas. No zip portátil, rode
`nuvia-helper.exe --install-service` como administrador uma vez.

O menu **REDE** também permite adicionar outras redes. Todas usam o **mesmo
adaptador `nuvia0` e o mesmo IP `10.240.x.y/16`**; cada sala tem sua porta UDP,
sessão mesh e chave de grupo. Até 8 pessoas por sala e várias salas ao mesmo
tempo. Como o TAP é compartilhado, o Minecraft vê as LANs de todas as salas
ativas. O botão **ON/OFF** da barra conecta/desconecta a sala selecionada.
Clique no cartão para expandir a lista de participantes; **botão direito**:
**DESATIVAR** ou **SAIR**.

O menu **AJUDA** tem **Ajustes** (identidade, servidor fixo e privilégios) e
**Sobre**.

A GUI usa **mesh** para 2–8 participantes, tentando direto e relay em
paralelo. Cada participante na lista expandida mostra um ponto: verde = direto,
roxo = relay, cinza = offline/aguardando. A GUI usa o mesmo
`%APPDATA%\nuvia\identity.toml` do CLI. As redes
ficam salvas em `%APPDATA%\nuvia\rooms.toml`. Fechar a janela esconde para a
bandeja; clique no ícone para abrir ou use **Sair** no menu da bandeja/AJUDA.

## 1. Instalar o driver (zip portátil)

O instalador já faz isso. Para o zip portátil, abra um PowerShell **como
Administrador** e rode:

```powershell
Set-ExecutionPolicy -Scope Process Bypass -Force
.\install-tap.ps1 -DriverDir .\driver
```

O zip inclui o driver `9.27.0` em `driver\`; sem `-DriverDir`, o script baixa o
release oficial do OpenVPN. A instalação usa
`devcon.exe install OemVista.inf TAP0901`. O Nuuvia reutiliza/cria um único
adaptador `nuvia0` sob demanda; não é preciso criar nada à mão.

## 2. Identidade

A GUI cria a identidade automaticamente no primeiro uso:

```
%APPDATA%\nuvia\identity.toml
```

Para gerar pelo CLI:

```powershell
.\nuvia.exe keygen --name pc-windows
.\nuvia.exe pubkey
```

A mesma identidade vale para todas as redes.

## 3. CLI (avançado)

Execute em um PowerShell **como Administrador**.

### Internet (sem convite)

O token do relay e o IP virtual são derivados do nome/senha. Não precisa de
`--ip` nem de `--room-token`:

```powershell
.\nuvia.exe up `
  --room casa --password 'troque-esta-senha' `
  --server https://167.234.237.6.sslip.io
```

No outro PC, use o mesmo nome e senha. O menor `public_key` fica com `.1` e o
maior com `.2`, dentro de `10.240.<octeto-da-sala>.0/24`.

### LAN manual

```powershell
.\nuvia.exe up `
  --room casa --password 'troque-esta-senha' `
  --tap-name nuvia0 --ip 10.240.0.2/24 `
  --bind 0.0.0.0:42424 `
  --peer <PUB_A>@192.168.1.10:42424
```

Use `--relay` quando um dos lados estiver em CGNAT/celular (4G/5G);
`--direct` força só o caminho P2P. Sem essas flags o CLI usa AUTO.

Teste no outro PC:

```powershell
ping 10.240.0.1
```

## O que é configurado automaticamente

- um único adaptador `nuvia0` com o IP virtual e MTU 1360;
- métrica da interface em **1** (para o multicast ganhar da Ethernet física);
- rotas `224.0.0.0/4`, `224.0.2.60/32` (Minecraft) e `255.255.255.255/32` pelo
  TAP da rede ativa (descoberta de jogos);
- perfil de rede do TAP como **Private**;
- uma regra de firewall de entrada `nuvia-nuvia0`;
- regra de firewall de entrada para o próprio `nuvia.exe`/`nuvia-gui.exe`
  (`nuvia-app`).

No CLI, se quiser desativar as rotas de descoberta (por exemplo, para não afetar
Chromecast/impressoras):

```powershell
--no-broadcast-routes
```

Na GUI, as rotas pertencem só à rede ativa. Para liberar a descoberta local,
desconecte a rede ativa ou ative outra.

## Regras importantes

- **Abra o jogo depois do Nuuvia.** Se o jogo já estava aberto, reinicie para ele
  fazer o join multicast no adaptador certo.
- Rode o Nuuvia como Administrador; criar adaptador e mexer em rotas/firewall
  exige privilégio.
- O Windows pode mostrar um alerta de SmartScreen porque o binário ainda não
  tem assinatura de código; isso fica para uma versão futura.
- O perfil do adaptador precisa ficar **Private**. O `doctor` e os logs mostram
  se o PowerShell não conseguiu ajustar.
- O firewall do jogo deve permitir rede **Private**. O TAP é marcado assim.
- Todas as redes usam o mesmo TAP `nuvia0` e a mesma regra de firewall; fechar
  uma rede não derruba as outras.

## Diagnóstico

Se a janela fechar sozinha, o log fica em
`%APPDATA%\nuvia\nuvia.log` (o executável é GUI, sem console).

O log também mostra qual renderer foi usado (`Glow`, `Wgpu` ou o fallback por
software). Em PCs antigos, se a janela não abrir, teste `NUVIA_RENDERER=glow` e
`NUVIA_RENDERER=wgpu` para isolar um problema de driver de vídeo. O app tenta o
outro renderer automaticamente; se ambos falharem, ele mostra no log
`relaunching with software OpenGL` e abre a cópia em `soft\` com Mesa3D
llvmpipe. Também há um atalho **Nuuvia VPN (compatibilidade)** no menu Iniciar
que abre essa cópia diretamente.

O `soft\` inclui `opengl32.dll` e `libgallium_wgl.dll` do Mesa3D 26.3.0. Eles são
distribuídos sem modificação e a licença está em `soft\MESA-LICENSE.txt`.

Se o log mostrar `direct tunnel failed: ... os error 2` ou `TAP indisponível`,
o driver `tap-windows6` não está instalado/registrado nesse PC. Rode como
administrador:

```powershell
cd "C:\Program Files\nuvia"
Set-ExecutionPolicy -Scope Process Bypass -Force
.\install-tap.ps1 -DriverDir .\driver
```

Se já existirem vários adaptadores "Conexão Local" / "TAP-Windows Adapter V9"
de tentativas antigas, limpe-os antes (isso também remove TAPs de OpenVPN, se
houver):

```powershell
.\reset-tap.ps1
.\install-tap.ps1 -DriverDir .\driver
```

```powershell
.\nuvia.exe doctor
Get-NetAdapter | Where-Object Name -like 'nuvia*'
Get-NetRoute -InterfaceAlias 'nuvia0'
Get-NetFirewallRule -DisplayName 'nuvia-*'
```

Se o adaptador não aparecer, rode o instalador do driver novamente e reinicie o
Windows.

Se a descoberta do jogo não funcionar, veja qual interface o Windows escolhe
para o grupo do Minecraft:

```powershell
Find-NetRoute -RemoteIPAddress 224.0.2.60 |
  Select-Object IPAddress,InterfaceAlias,RouteMetric
```

Precisa aparecer o TAP compartilhado (`nuvia0`). Se aparecer
`Ethernet` ou outro adaptador virtual:

```powershell
Set-NetIPInterface -InterfaceAlias nuvia0 -InterfaceMetric 1
Get-NetRoute -DestinationPrefix 224.0.2.60/32 -ErrorAction SilentlyContinue |
  Remove-NetRoute -Confirm:$false
New-NetRoute -DestinationPrefix 224.0.2.60/32 `
  -InterfaceAlias nuvia0 -RouteMetric 1
```

Depois feche o mundo e abra de novo com **Open to LAN**; o socket multicast do
host só troca de interface quando o mundo é reaberto.
