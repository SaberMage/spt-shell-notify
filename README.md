# spt-shell-notify

OS-notification shell adapter for [spt-core] — renders agent commands and
surfaced subnet notifs as **native OS notifications** (Windows toast / Linux
`notify-send`).

This is the reference *real* shell adapter and the proof of spt-core's
adapter model: **the manifest + one binary are the only glue.** No spt-core
crates, no source integration — the binary speaks the public `spt api`
command surface and the documented EVENT envelope wire format, and the
manifest declares everything spt-core needs to host it.

## What it does

- `spt shell cmd <id> notify <title> <body>` → a native toast, driven by the
  owner agent down the durable command channel (the resident binary drains
  `api poll --link` and renders).
- `spt notify <body>` anywhere in the subnet → presence resolution picks the
  node the user last touched → spt-core spawns this adapter's
  `[session.notif]` template there → native toast. No agent in the loop.

## Install

```
cargo install --path .          # puts notify-shell on PATH
spt adapter add <this dir>      # registers the manifest
spt shell spawn notify          # mints + launches an instance
```

## Layout

- `manifest.toml` — the whole spt-core contract: spawn/wake/close templates,
  the one-verb `notify` capability, the `[session.notif]` render template.
- `src/main.rs` — the binary: resident shell mode, one-shot `--render-*`
  mode (the notif template), `--wake` watcher mode. Self-contained envelope
  decode (amp-last, `<br>`-first — the documented order).

## Testing

`cargo test` covers frame parsing and the PowerShell embedding. The
end-to-end leg (spawn → bind → command → render) lives in spt-core's CI as an
env-gated test: it clones this repo, builds the binary, and drives it through
the real `spt` surface (`SPT_NOTIFY_SHELL_BIN`).

[spt-core]: https://github.com/SaberMage/spt-core
