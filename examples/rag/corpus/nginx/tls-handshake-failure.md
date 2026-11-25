+++
title = "Nginx TLS 握手失败排查"
source = "https://nginx.org/en/docs/http/configuring_https_servers.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_nginx01", "srv_nginx02"]
acl = ["public"]
+++

# Nginx TLS 握手失败排查

## 现象 {#symptoms}

客户端报 `SSL_do_handshake() failed`、`no shared cipher`、`unknown ca`、`certificate verify failed` 或浏览器提示证书错误。Nginx 错误日志中的 TLS 告警通常只给出握手阶段，必须结合客户端、SNI、证书链和协议版本判断。

## 握手失败分类 {#handshake-failure}

- 证书链不完整：服务端只发送叶证书，客户端找不到中间 CA。
- SNI 不匹配：多个站点共用一个监听地址，但请求的 `server_name` 没有命中正确 server block。
- 证书过期、主机名不匹配或系统时间错误。
- 客户端与服务端没有共同协议或密码套件。
- 客户端证书校验失败，mTLS 的 CA 或用途配置不一致。
- 前置负载均衡终止了 TLS，故障其实发生在其与客户端或后端之间。

## 证书链检查 {#certificate-chain}

使用受控客户端检查实际返回的证书和链：

```bash
openssl s_client -connect demo-nginx-01.example.test:443 \
  -servername demo-nginx-01.example.test -showcerts </dev/null
openssl x509 -in server.crt -noout -subject -issuer -dates -ext subjectAltName
```

确认 `ssl_certificate` 包含叶证书和必要的中间证书，顺序正确，根证书通常不需要由服务端发送。私钥不得出现在日志或工单中。证书更新后检查权限、文件路径和 `nginx -t`；若使用自动续期，还要确认 reload 钩子确实执行。

## 协议、SNI 与密码套件 {#protocol-ciphers}

检查 `listen 443 ssl`、`server_name`、`ssl_protocols` 和 `ssl_ciphers`。不要为了兼容旧客户端无条件重新启用 TLS 1.0/1.1。若同 IP 不同端口、不同 SNI 命中不同证书，逐个测试目标域名。上游代理场景要区分客户端到 Nginx 与 Nginx 到上游的 TLS，两者的证书和 CA 信任库可能不同。

## 验证 {#verification}

用浏览器、命令行客户端和自动化健康检查分别验证，确认证书链、SNI、协议、过期时间和主机名都匹配。查看 Nginx 错误日志中是否仍有握手告警，并用 `curl -vI https://<host>/<path>` 检查实际协商结果。变更应保留旧证书和旧配置，直到观察窗口结束。
