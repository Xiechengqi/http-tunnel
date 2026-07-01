# Cloudflare

Create proxied `A` records for both `@` and `*` pointing at the server IP. Cloudflare handles public TLS.

Recommended DNS:

```text
A  @  <server_ip>  Proxied
A  *  <server_ip>  Proxied
```

Use `public_scheme = "https"` when traffic reaches users through Cloudflare HTTPS. The origin can start as HTTP; Cloudflare terminates public TLS.

WebSocket proxying must be enabled for the zone. Public WebSocket requests only receive `101 Switching Protocols` after the local target accepts the upgrade through the tunnel.
