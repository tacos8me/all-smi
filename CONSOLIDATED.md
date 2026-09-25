# Consolidated Mac + NVIDIA view

This branch (`consolidated` on [tacos8me/all-smi](https://github.com/tacos8me/all-smi/tree/consolidated))
extends all-smi so that a Mac Studio (M5 Ultra, unified memory) and an NVIDIA
box (2x RTX PRO 6000 Blackwell) serving one model together can be watched as
one system: one screen, every accelerator, combined memory and power, the
link between the machines, and the state of the serving pipeline.

## What it adds

| Area | Change |
|---|---|
| `all-smi view --consolidated` | A **Consolidated** tab, opened at startup: one row per accelerator across all `--hosts` with utilization, memory labeled `unified` or `VRAM`, power against the board limit, temperature, clock, and utilization/power sparklines; an ANE/CPU rail line under Apple GPUs; a combined total (memory across unified + VRAM, SoC vs discrete GPU power, average utilization, sparklines); LINK and LOCK rows from the exporters' host probes; a row with the scrape error for any host that stops answering. `C` jumps to the tab. |
| `all-smi view --icculus` | Adds the **Icculus pipeline** panel to that tab (implies `--consolidated`): the box engine's `/health` (status, build, numerics, uptime, sessions, connections, the GPU job in flight and its age, queue depth, prefix-cache hit ratio and size, with busy/queue sparklines), llama-swap `/running` on the Mac (loaded model and state), the Mac `gpu.lock` holder, and the 10GbE link rates at both ends. Endpoints: `--icculus-health URL` (default `http://10.10.10.1:10051/health`), `--icculus-swap URL` (default `http://10.10.10.2:8080`). |
| `all-smi api --bind ADDR[,ADDR]` | One listener per address instead of `0.0.0.0`, so an exporter can stay off the LAN. |
| `all-smi api --net-iface IF` | Exports `all_smi_network_{receive,transmit}_bytes_total` and per-interval `..._bytes_per_second` for that interface. |
| `all-smi api --watch-lock PATH` | Exports `all_smi_lock_held`, `all_smi_lock_holder_info{pid,command}` and `all_smi_lock_held_since_seconds`, found with `lsof`/`ps`; the lock is never opened or taken. |
| Fixes | Apple M5 Ultra CPU/ANE/DRAM power no longer reads 0 (see below); `view --hosts http://host:port` now joins tabs, connection status and devices (host tabs used to show "CONNECTION LOST" for live nodes); the LED grid no longer counts the Topology tab as a node; the viewer parses the Apple GPU core count; a lock holder that predates the exporter is dated from its process start. |

A stock exporter's `/metrics` output is unchanged unless the new flags are
used, and the tab only appears with `--consolidated` / `--icculus`.

## Run it

Exporters (both already installed as services on this pair, see below):

```sh
# Mac Studio (M5 Ultra), no sudo
all-smi api --port 9090 --bind 10.10.10.2,127.0.0.1 --interval 3 \
    --net-iface en0 --watch-lock ~/llm/locks/gpu.lock
# RTX box
all-smi api --port 9090 --bind 10.10.10.1,127.0.0.1 --interval 3 \
    --net-iface enp161s0f0np0
```

Viewer (from the box, or from the Mac over the link):

```sh
all-smi view --hosts http://10.10.10.2:9090 http://10.10.10.1:9090 --consolidated --icculus
```

`←`/`→` switch tabs, `C` returns to the Consolidated tab, `h` shows help,
`q` quits. Add `--interval N` to change the scrape cadence (the pipeline
poller never polls `/health` faster than every 2 s, and llama-swap's
`/running` at most every 10 s because llama-swap logs each request).

The same settings can live in the config file (`all-smi config path`):

```toml
[api]
bind = ["10.10.10.1", "127.0.0.1"]
net_interfaces = ["enp161s0f0np0"]
watch_locks = []            # e.g. ["/Users/ian/llm/locks/gpu.lock"] on the Mac

[consolidated]
enabled = true              # same as --consolidated
icculus = true              # same as --icculus
icculus_health_url = "http://10.10.10.1:10051/health"
icculus_swap_url = "http://10.10.10.2:8080"
```

## What it looks like

Captured with `tmux capture-pane` from a 160x48 terminal on the box running
`all-smi view --hosts http://10.10.10.2:9090 http://10.10.10.1:9090 --consolidated --icculus`
(the stock Cluster Overview and Live Statistics rows above the tab strip are
omitted). RTX #0 was busy with another workload while the engine was idle:

```
Tabs:  All  Consolidated  Users  Topology  ians-Mac-Studio.local  vllm
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
Consolidated  3 accelerators on 2 hosts (2 up) as one system
HOST             DEVICE                           UTIL MEMORY                         POWER  TEMP     CLOCK  UTIL HISTORY              POWER HISTORY
ians-Mac-Studio  M5 Ultra GPU (80 cores)          4.3% 153.7/256.0 GiB unified       0.02 W  25°C   600 MHz  ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀ ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
                 └ ANE 0.00 W · CPU 2.7 W
vllm             RTX PRO 6000 Blackwell #0      100.0% 91.1/95.6 GiB VRAM         136/600 W  35°C  2962 MHz  ⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿ ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
                 RTX PRO 6000 Blackwell #1        0.0% 91.1/95.6 GiB VRAM        96.5/600 W  33°C  2610 MHz  ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀ ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
TOTAL            3 accelerators                  34.8% 336.0/447.2 GiB 75%            232 W                  ⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤ ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
                 unified 153.7/256.0 GiB, SoC GPU 0.02 W · VRAM 182.2/191.2 GiB, discrete GPUs 232 W

── Icculus pipeline ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
ENGINE           vllm :10051           ok · f1b5ebe · og-s4.3 · up 1h 16m
                                       sessions 0 · connections 1 · gpu idle · queue 0  busy ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀  queue ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
                                       prefix cache 47/73 hits (64.4%) · 846 entries · 29.6/64.0 GiB · 291.3K tokens resumed
SWAP             ians-Mac-Studio :8080 ds41 ready  Icculus · DeepSeek-V4.1-Flash ORIGINAL FP4/FP8 (RTX box layers 0-19 + Mac 20-39) + DSpark
LOCK             ians-Mac-Studio       ~/llm/locks/gpu.lock  held by omlx-server (pid 72176) for 39m 29s
LINK             ians-Mac-Studio       en0             ↓    257 B/s   ↑   6.7 KB/s   ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀ ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
LINK             vllm                  enp161s0f0np0   ↓   7.6 KB/s   ↑    280 B/s   ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀ ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
```

With the Mac exporter unreachable, its row turns into
`10.10.10.2:9090  unreachable: <scrape error>`, the totals shrink to the
hosts that answered, and the engine and llama-swap rows show
`unreachable: <reason>` independently.

## Deployment on this pair

| Machine | Service | Listens on | Probes |
|---|---|---|---|
| RTX box (`vllm`) | systemd user unit `all-smi-api` (`~/.config/systemd/user/all-smi-api.service`, from `packaging/consolidated/all-smi-api.service`; linger is on) | `10.10.10.1:9090`, `127.0.0.1:9090` | `enp161s0f0np0` |
| Mac Studio | launchd user agent `com.tacos8me.all-smi-api` (`~/Library/LaunchAgents/`, from `packaging/consolidated/com.tacos8me.all-smi-api.plist`), bootstrapped into `user/501` | `10.10.10.2:9090`, `127.0.0.1:9090` | `en0`, `~/llm/locks/gpu.lock` |

Binaries are at `~/.local/bin/all-smi` on both machines. Manage them with
`systemctl --user {status,restart} all-smi-api` on the box and
`launchctl kickstart -k user/$(id -u)/com.tacos8me.all-smi-api` /
`launchctl bootout user/$(id -u)/com.tacos8me.all-smi-api` on the Mac.
Mac logs go to `~/Library/Logs/all-smi-api.log`; box logs to the journal.

Footprint: 3 s cadence, `RUST_LOG=warn` (the API otherwise logs every
request at debug level), niced, idle I/O class, and on the box
`CPUQuota=25%`/`MemoryMax=256M`. NVML is only queried, never configured
(the process list is off). Measured: about 0.2 % of a core and 33 MB on
the box, about 1.5 % of a core and 20 MB on the Mac.

## Apple M5 Ultra power readings

This headless M5 Ultra publishes its CPU, ANE and DRAM energy counters once
every 30 minutes (the M5 Max and M1 Ultra publish every 1-2 s), so upstream's
10 s hold showed them as 0 W almost all the time. The branch holds a reading
for two of the channel's own publication spans (never less than 10 s):
CPU/ANE/DRAM power on this Mac is therefore a half-hour average, and reads 0
from an exporter restart until the next half-hourly publication. GPU power
(`GPU Energy`) is live. The captured channel inventory is in
`tests/fixtures/ioreport/m5_ultra_energy_model.tsv`.

## Build

```sh
cargo build --release          # Linux needs protoc (PROTOC=/path/to/protoc)
cargo test                     # unit tests for probes, parser, model, renderer, poller
```

## Known limitations

- A launchd *user* agent starts with a login session. The Mac runs headless
  with no console login, so after a reboot the agent is not running until a
  GUI login loads it or someone runs
  `launchctl bootstrap user/$(id -u) ~/Library/LaunchAgents/com.tacos8me.all-smi-api.plist`
  (for example over SSH). Starting at boot with nobody logged in needs the
  same plist installed once as a LaunchDaemon with `UserName`, which needs
  sudo.
- The Consolidated tab is built for HTTP `--hosts` mode; `--ssh` and
  `--replay` views do not add it.
- "Held for" on the LOCK row uses the holder's process start when the
  holder predates the exporter; that is exact for servers that take the lock
  at startup and an upper bound otherwise.
