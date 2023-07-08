[![CI](https://github.com/gngpp/opengpt/actions/workflows/CI.yml/badge.svg)](https://github.com/gngpp/opengpt/actions/workflows/CI.yml)
<a href="/LICENSE">
    <img src="https://img.shields.io/github/license/gngpp/opengpt?style=flat">
  </a>
  <a href="https://github.com/gngpp/opengpt/releases">
    <img src="https://img.shields.io/github/release/gngpp/opengpt.svg?style=flat">
  </a><a href="hhttps://github.com/gngpp/opengpt/releases">
    <img src="https://img.shields.io/github/downloads/gngpp/opengpt/total?style=flat&?">
  </a>
  [![Docker Image](https://img.shields.io/docker/pulls/gngpp/opengpt.svg)](https://hub.docker.com/r/gngpp/opengpt/)

<br>简体中文 | [English](README_en.md)

# opengpt

不仅仅是非官方的 ChatGPT 代理（绕过 Cloudflare 403 Access Denied）

- API密钥获取，电子邮件/密码帐户认证 (由于作者没有账号，暂不支持Google/微软第三方登录)
- Http API 代理 (供其他客户端接入)
- 原汁原味ChatGPT WebUI

> 局限性: 这无法绕过 OpenAI 的彻底 IP 禁令

### Compile

> Ubuntu机器为例:

```shell

sudo apt update -y && sudo apt install rename

# Native compilation
git clone https://github.com/gngpp/opengpt.git && cd opengpt
./build.sh

# Cross-platform compilation, relying on docker (if you can solve cross-platform compilation dependencies on your own)
./corss-build.sh
```

### Ubuntu(Other Linux)

GitHub [Releases](https://github.com/gngpp/opengpt/releases/latest) 中有预编译的 deb包，二进制文件，以Ubuntu为例：

```shell
wget https://github.com/gngpp/opengpt/releases/download/v0.1.1/opengpt-0.1.1-x86_64-unknown-linux-musl.deb

dpkg -i opengpt-0.1.1-x86_64-unknown-linux-musl.deb

opengpt serve
```

### Docker

```shell
docker run --rm -it -p 7999:7999 --hostname=opengpt \
  -e OPENGPT_TLS_CERT=/path/to/cert \
  -e OPENGPT_TLS_KEY=/path/to/key \
  gngpp/opengpt:latest opengpt serve
```

### Command Line(dev)

### Http Server

- Comes with original ChatGPT WebUI
- Support unofficial/official API, forward to proxy
- The API prefix is the same as the official one, only the host name is changed

- API文档
  - Platfrom API [doc](https://platform.openai.com/docs/api-reference)
  - Backend API [doc](doc/rest.http)

- 参数说明
  - `--host`，环境变量 `OPENGPT_HOST`， 服务监听地址: 默认0.0.0.0，
  - `--port`，环境变量 `OPENGPT_PORT`， 监听端口: 默认7999
  - `--workers`，环境变量 `OPENGPT_WORKERS`， 服务线程数: 默认1
  - `--level`，环境变量 `OPENGPT_LOG_LEVEL`，日志级别: 默认info
  - `--tls-cert`，环境变量 `OPENGPT_TLS_CERT`，TLS证书公钥，支持格式: EC/PKCS8/RSA
  - `--tls-key`，环境变量 `OPENGPT_TLS_KEY`，TLS证书私钥
  - `--proxy`，环境变量 `OPENGPT_PROXY`，代理，格式: protocol://user:pass@ip:port

```shell
$ opengpt serve --help
Start the http server

Usage: opengpt serve [OPTIONS]

Options:
  -H, --host <HOST>
          Server Listen host [env: OPENGPT_HOST=] [default: 0.0.0.0]
  -P, --port <PORT>
          Server Listen port [env: OPENGPT_PORT=] [default: 7999]
  -W, --workers <WORKERS>
          Server worker-pool size (Recommended number of CPU cores) [env: OPENGPT_WORKERS=] [default: 1]
  -L, --level <LEVEL>
          Log level (info/debug/warn/trace/error) [env: OPENGPT_LOG_LEVEL=] [default: info]
      --proxy <PROXY>
          Server proxy, example: protocol://user:pass@ip:port [env: OPENGPT_PROXY=]
      --tcp-keepalive <TCP_KEEPALIVE>
          TCP keepalive (second) [env: OPENGPT_TCP_KEEPALIVE=] [default: 5]
      --tls-cert <TLS_CERT>
          TLS certificate file path [env: OPENGPT_TLS_CERT=]
      --tls-key <TLS_KEY>
          TLS private key file path (EC/PKCS8/RSA) [env: OPENGPT_TLS_KEY=]
  -S, --sign-secret-key <SIGN_SECRET_KEY>
          Enable url signature (signature secret key) [env: OPENGPT_SIGNATURE=]
  -T, --tb-enable
          Enable token bucket flow limitation [env: OPENGPT_TB_ENABLE=]
      --tb-store-strategy <TB_STORE_STRATEGY>
          Token bucket store strategy (mem/redis) [env: OPENGPT_TB_STORE_STRATEGY=] [default: mem]
      --tb-capacity <TB_CAPACITY>
          Token bucket capacity [env: OPENGPT_TB_CAPACITY=] [default: 60]
      --tb-fill-rate <TB_FILL_RATE>
          Token bucket fill rate [env: OPENGPT_TB_FILL_RATE=] [default: 1]
      --tb-expired <TB_EXPIRED>
          Token bucket expired (second) [env: OPENGPT_TB_EXPIRED=] [default: 86400]
  -h, --help
          Print help
```

### 参考

- <https://github.com/tjardoo/openai-client>
- <https://github.com/jpopesculian/reqwest-eventsource>
