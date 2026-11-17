# Deploy na Oracle Cloud

O servidor de rendezvous roda em uma VPS pequena. Ele **não** carrega o tráfego
L2 no M5a; só recebe registro HTTPS, descobre endpoints públicos por UDP e
devolve a lista de peers. O relay (M5b) usará a mesma VPS.

A chave SSH da instância está em
`~/.ssh/nuvia/ssh-key-2026-11-16.key` (modo 600) e nunca é versionada.

## Instância atual

- IP público reservado: **167.234.237.6**
- URL TLS: **https://167.234.237.6.sslip.io**
- UDP do rendezvous: `167.234.237.6.sslip.io:8443`
- Região: `sa-saopaulo-1`
- Usuário: `ubuntu`
- Ubuntu 20.04.6 LTS, x86_64, `VM.Standard.E2.1.Micro` (1 OCPU, 1 GB)
- OCID: `ocid1.instance.oc1.sa-saopaulo-1.antxeljrl56in6icuxmn7eqkjswjxi6gf4if3oe54pbd5j3axqz3ycdvmo4a`

O servidor foi implantado como binário estático musl; `/v1/healthz` responde
por HTTPS e o UDP 8443 responde ao hello. A chave SSH está em
`~/.ssh/nuvia/ssh-key-2026-11-16.key` (modo 600).

O **relay M5b** está ativo no mesmo UDP 8443: ele valida sala/token, atualiza o
endpoint do remetente e encaminha apenas o pacote interno já cifrado. Não há
porta nova para abrir. O cliente usa `--relay` quando um dos lados está em
CGNAT/celular; sem a flag ele tenta o caminho direto.

Ainda falta validar o túnel entre duas redes diferentes e implementar o relay
do M5b. Um domínio próprio é opcional; o `sslip.io` já tem certificado
Let's Encrypt de produção.

## 1. Rede na Oracle

No console da OCI, na VCN da instância:

- **Reserved public IP**: associe um IP reservado. Sem isso, desligar/ligar a
  instância pode trocar o IP e quebrar o DNS/TLS.
- Security list / NSG (ingress):
  - TCP 22 do seu IP;
  - TCP 80 de `0.0.0.0/0` (Let's Encrypt);
  - TCP 443 de `0.0.0.0/0` (API HTTPS);
  - UDP 8443 de `0.0.0.0/0` (descoberta/relay).
- Egress pode ficar aberto. O Always Free inclui 10 TB/mês de saída.

No guest, o `bootstrap.sh` configura UFW com as mesmas portas. Se a imagem
Ubuntu da Oracle tiver regras de iptables antigas, confira:

```bash
sudo iptables -S
sudo ufw status verbose
```

## 2. DNS

Crie um registro A:

```
relay.seudominio.com -> <IP público reservado>
```

O Caddy emite o certificado Let's Encrypt automaticamente quando o DNS já
aponta para a VPS e as portas 80/443 estão abertas.

Sem domínio, dá para usar `IP.sslip.io` (por exemplo
`203.0.113.10.sslip.io`). O Caddy consegue emitir para esse nome, mas um domínio
próprio é mais estável.

## 3. Testar o SSH

```bash
ssh -i ~/.ssh/nuvia/ssh-key-2026-11-16.key ubuntu@<IP>
uname -m
cat /etc/os-release | head -3
```

## 4. Primeiro deploy

Na sua máquina, dentro do projeto:

```bash
export VPS_HOST=<IP>
export VPS_USER=ubuntu
export DOMAIN=relay.seudominio.com
./deploy/oracle/publish.sh --bootstrap
```

O script:

1. envia o fonte para `/tmp/nuvia-src` na VPS;
2. roda `bootstrap.sh` (Caddy, usuário `nuvia`, systemd, UFW);
3. compila o `nuvia-server` **aqui** para `x86_64-unknown-linux-musl`
   (binário estático, não precisa de Rust na VPS);
4. envia o binário e instala em `/usr/local/bin/nuvia-server`;
5. reinicia o serviço e consulta `/v1/healthz`.

Se preferir compilar na VPS (outra arquitetura ou sem musl local):

```bash
BUILD=remote VPS_HOST=... ./deploy/oracle/publish.sh
```

Nesse caso instale `build-essential` e adicione swap, porque o shape de 1 GB
pode ficar apertado.

Atualizações depois:

```bash
VPS_HOST=<IP> ./deploy/oracle/publish.sh
```

## 5. Verificar

```bash
curl https://relay.seudominio.com/v1/healthz
```

Resposta esperada:

```json
{"status":"ok","protocol":1,"rooms":0,"peers":0,"public_url":"https://relay.seudominio.com"}
```

Logs:

```bash
ssh -i ~/.ssh/nuvia/ssh-key-2026-11-16.key ubuntu@<IP> \
  'sudo journalctl -u nuvia-server -f'
ssh -i ~/.ssh/nuvia/ssh-key-2026-11-16.key ubuntu@<IP> \
  'sudo journalctl -u caddy -f'
```

## 6. Criar o convite

No seu PC (não precisa ser na VPS):

```bash
./target/release/nuvia invite --server https://relay.seudominio.com --room casa
```

Copie o código e a senha da sala. Cada peer usa um IP virtual diferente:

```bash
sudo ./target/release/nuvia \
  --identity "$HOME/.config/nuvia/identity.toml" \
  up --invite '<CODIGO>' --password 'troque-esta-senha' \
  --ip 10.240.0.1/24
```

O outro lado usa `10.240.0.2/24`.

## 7. Troubleshooting

| Sintoma | Causa provável |
|---|---|
| Caddy não emite certificado | DNS ainda não aponta para a VPS, portas 80/443 fechadas no security list/NSG |
| `curl /v1/healthz` timeout | Caddy ou nuvia-server parados; `journalctl` |
| Portas 80/443/8443 dão timeout | Security list/NSG da Oracle não liberou; confira os dois |
| Portas 80/443/8443 dão connection refused | REJECT do iptables da imagem Oracle antes do UFW; o bootstrap novo remove, ou rode os comandos abaixo |
| Peers não se encontram | UDP 8443 fechado no security list/NSG ou no guest firewall |
| `peers: 0` no health | clientes não estão registrando; confira a URL do convite |
| Build morre na VPS | pouca RAM; use o binário estático local (padrão) ou adicione swap |

Se o guest for uma imagem Oracle Ubuntu antiga e as portas derem **connection
refused**, remova o REJECT que fica antes das chains do UFW:

```bash
sudo iptables -D INPUT -j REJECT --reject-with icmp-host-prohibited
sudo iptables -D FORWARD -j REJECT --reject-with icmp-host-prohibited
sudo sed -i '/^-A INPUT -j REJECT --reject-with icmp-host-prohibited$/d' /etc/iptables/rules.v4
sudo sed -i '/^-A FORWARD -j REJECT --reject-with icmp-host-prohibited$/d' /etc/iptables/rules.v4
sudo netfilter-persistent save
sudo systemctl restart caddy
```

Não remova as regras `InstanceServices` (serviço de metadata) do
`/etc/iptables/rules.v4`.

O servidor não conhece a senha da sala e não tem as chaves de tráfego. No M5a
ele só vê IDs, chaves públicas, endpoints e o token do convite. No M5b ele
encaminhará pacotes cifrados, sem conseguir decifrá-los.
