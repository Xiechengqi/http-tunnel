# Cloudflare

Create proxied `A` records for both `@` and `*` pointing at the server IP. Cloudflare handles public TLS.

Recommended DNS:

```text
A  @  <server_ip>  Proxied
A  *  <server_ip>  Proxied
```

Use `public_scheme = "https"` when traffic reaches users through Cloudflare HTTPS. The origin can start as HTTP; Cloudflare terminates public TLS.

The default trusted proxy CIDRs include Cloudflare IP ranges, so tunnel WebSocket sessions can use `CF-Connecting-IP` for the client IP. Dashboard country data still comes only from the client report; the server ignores proxy country headers. Existing config files keep their saved `trusted_proxy_cidrs`; add the Cloudflare ranges or reset the field if the config was created before this behavior.

WebSocket proxying must be enabled for the zone. Public WebSocket requests only receive `101 Switching Protocols` after the local target accepts the upgrade through the tunnel.
