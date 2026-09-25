# Consolidated Mac + NVIDIA view

This branch (`consolidated` on [tacos8me/all-smi](https://github.com/tacos8me/all-smi/tree/consolidated))
extends all-smi so that a Mac Studio (M5 Ultra, unified memory) and an NVIDIA
box (2x RTX PRO 6000 Blackwell) serving one model together can be watched as
one system: one screen, every accelerator, combined memory and power, the
link between the machines, and the state of the serving pipeline.

## What it adds

| Area | Change |
|---|---|
| Remote header (every tab) | A title line (`all-smi  cluster`, hosts up, clock, version) and one row of overview blocks: accelerators (SoC vs discrete), average GPU utilization, and memory split by where it lives: **UNIFIED MEMORY** (an Apple Silicon host's RAM, which is its GPU's pool, counted once), **VRAM** (discrete GPUs) and **HOST RAM** (the other hosts), then power against the summed board limits and the hottest device against its slowdown threshold. A HOSTS block appears only to name hosts that are down. Replaces the Cluster Overview cells, the Live Statistics sparklines and the node LED grid. |
| All tab | Accelerators grouped under a line per host (OS, chip or CPU, CPU load, RAM), one aligned row per device: full name (`Apple M5 Ultra · 80-core GPU`, `NVIDIA RTX PRO 6000 Blackwell #0`), utilization and memory bars in one style, memory kind, temperature, power against the limit, clock. Columns drop in a fixed order as the terminal narrows (clock, memory kind, name width, power, temperature). Thermal thresholds, P-state, driver, CUDA, GSP firmware and PCIe move behind `x`. The rest of the screen is a history panel: per device, utilization, memory in use and power charts sized to the space left (cluster-wide charts when the devices do not fit). With `--icculis`, a one-line Icculis strip sits between the table and the history. |
| Host tabs | The host's group from the All tab with the details line on, its disks as usage bars, and its devices' history. |
| Palette | `ui::theme`: zinc greys for structure, one muted teal accent for headings, the selected tab and active pipeline work, and green / amber / red only for a reading past a threshold. `NO_COLOR` turns every color off (the selected tab falls back to reverse video). |
| `all-smi view --consolidated` | A **Consolidated** tab, opened at startup: one row per accelerator across all `--hosts` with utilization, memory labeled `unified` or `VRAM`, power against the board limit, temperature, clock, and utilization/power sparklines; an ANE/CPU rail line under Apple GPUs; a combined total; LINK and LOCK rows from the exporters' host probes; a row with the scrape error for any host that stops answering; the history panel below. `C` jumps to the tab. |
| `all-smi view --icculis` | Adds the **Icculis** panel (implies `--consolidated`; `--icculus` still works): a diagram of the split with a live phase on each half and the link between them, then the engine's `/health` (sessions, connections, queue, prefix cache, busy/queue sparklines), llama-swap `/running` on the Mac, the Mac `gpu.lock` holder and the 10GbE link rates. Endpoints: `--icculis-health URL` (default `http://10.10.10.1:10051/health`), `--icculis-swap URL` (default `http://10.10.10.2:8080`). |
| `all-smi api --json-probe NAME=URL` | Exports the numeric fields of a loopback JSON status endpoint as `all_smi_json_probe_value{probe,key}`, their per-second change as `all_smi_json_probe_rate{probe,key}`, and `all_smi_json_probe_up{probe}`. One plain GET per cycle, `http://` only, 800 ms timeout, 256 KiB and 64 fields at most. The Mac agent uses it for the Icculis phase. |
| `all-smi api --bind ADDR[,ADDR]` | One listener per address instead of `0.0.0.0`, so an exporter can stay off the LAN. |
| `all-smi api --net-iface IF` | Exports `all_smi_network_{receive,transmit}_bytes_total` and per-interval `..._bytes_per_second` for that interface. |
| `all-smi api --watch-lock PATH` | Exports `all_smi_lock_held`, `all_smi_lock_holder_info{pid,command}` and `all_smi_lock_held_since_seconds`, found with `lsof`/`ps`; the lock is never opened or taken. |
| Fixes | Apple M5 Ultra CPU/ANE/DRAM power no longer reads 0 (see below); `view --hosts http://host:port` now joins tabs, connection status and devices; the viewer parses the Apple GPU core count; a lock holder that predates the exporter is dated from its process start. |

A stock exporter's `/metrics` output is unchanged unless the new flags are
used, and the Consolidated tab only appears with `--consolidated` / `--icculis`.

## Run it

Exporters (both already installed as services on this pair, see below):

```sh
# Mac Studio (M5 Ultra), no sudo
all-smi api --port 9090 --bind 10.10.10.2,127.0.0.1 --interval 3 \
    --net-iface en0 --watch-lock ~/llm/locks/gpu.lock \
    --json-probe sup=http://127.0.0.1:10001/health \
    --json-probe og=http://127.0.0.1:10001/og/stats \
    --json-probe omlx=http://127.0.0.1:10001/api/status
# RTX box
all-smi api --port 9090 --bind 10.10.10.1,127.0.0.1 --interval 3 \
    --net-iface enp161s0f0np0
```

Viewer (from the box, or from the Mac over the link):

```sh
all-smi view --hosts http://10.10.10.2:9090 http://10.10.10.1:9090 --icculis
```

Keys: `←`/`→` switch tabs, `C` jumps to the Consolidated tab, `x` shows the
device details (thermal thresholds, P-state, driver, CUDA, GSP, PCIe) on
the All tab, `↑`/`↓` scroll the device table, `d`/`u`/`g` sort devices
within each host (default, utilization, memory), `/` filters, `h` shows
help, `q` quits. Add `--interval N` to change the scrape cadence (the
pipeline poller never polls `/health` faster than every 2 s, and
llama-swap's `/running` at most every 10 s because llama-swap logs each
request).

The same settings can live in the config file (`all-smi config path`):

```toml
[api]
bind = ["10.10.10.1", "127.0.0.1"]
net_interfaces = ["enp161s0f0np0"]
watch_locks = []            # e.g. ["/Users/ian/llm/locks/gpu.lock"] on the Mac
json_probes = []            # e.g. ["og=http://127.0.0.1:10001/og/stats"] on the Mac

[consolidated]
enabled = true              # same as --consolidated
icculis = true              # same as --icculis (icculus = true also works)
icculis_health_url = "http://10.10.10.1:10051/health"
icculis_swap_url = "http://10.10.10.2:8080"
```

## Palette

| Role | 256-color | Meaning |
|---|---|---|
| Text | 254 (zinc-200) | values and names |
| Secondary | 248 (zinc-400) | units, clocks, detail values |
| Label | 243 (zinc-500) | labels, column headers, key hints |
| Rule | 238 (zinc-700) | rules, bar tracks, empty history |
| Accent | 73 (muted teal) | headings, the selected tab, utilization charts, a pipeline half that is working |
| OK | 71 (green) | bar fill of a reading in its normal range |
| Warn | 179 (amber) | utilization ≥ 70 %, memory ≥ 85 %, power ≥ 70 % of the limit, temperature within 15 °C of slowdown |
| Crit | 167 (red) | utilization ≥ 90 %, memory ≥ 95 %, power ≥ 90 %, temperature within 5 °C of slowdown, a host or endpoint that is down |

Numbers stay in the text color while normal; only bars carry green, so an
idle cluster shows five greys, the accent and green.

## What it looks like

Captured with `tmux capture-pane` from a 160x48 terminal on the box
running `all-smi view --hosts http://10.10.10.2:9090 http://10.10.10.1:9090`
(ds41 was not loaded; another job had just used the Mac GPU).

Before (the stock layout):

```
all-smi - 2026-09-25 22:43:07                                                                                                                            v0.26.3
Cluster Overview
│ Nodes       │ Total RAM   │ GPU Cores   │ Total VRAM  │ Avg. Temp   │ Total Power │  ○○
│ 2/2         │ 759GB       │ 3           │ 759GB       │ 30°C        │ 0.2kW       │
│ CPU Cores   │ Used RAM    │ GPU Util    │ Used VRAM   │ Thermal     │ Avg. Power  │
│ 72          │ 417GB       │ 27.4%       │ 417GB       │ Unknown     │ 70.4W       │
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
Live Statistics
GPU Util.                                                           ⢀⣸ 10.5%  CPU Util.                                                           ⢀⣄ 5.3%
GPU Mem.                                                            ⢠⣤ 81.7%  Host Mem.                                                           ⢠⣤ 56.2%
GPU Temp.                                                           ⢰⣶  30°C  CPU Temp.                                                           ⢠⣤  27°C

Tabs:  All  Users  Topology  ians-Mac-Studio.local  vllm
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
GPU  GPU   Apple M5  @ .local    Util:  4.2% VRAM:154.2/256GB Temp:  25°C Freq:600MHz Pwr:   0.02W
     Util : [▬──────────────────────────────    4.2%]  Mem  : [▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬──────── 154.2GB]  ANE  : [───────────────────────────────    0.0W]
GPU  6000 Blackwell  @ vllm      Util:  0.0% VRAM:  88.1/96GB Temp:  32°C Freq:2.61GHz Pwr: 93/600W
      Slowdown:95°C Shutdown:98°C MaxOp:93°C P-State:P1
      HW GSP:default v595.45.04
     Util : [─────────────────────────────────────────────────────────    0.0%]  Mem  : [▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬  88.1GB]
GPU  6000 Blackwell  @ vllm      Util: 78.0% VRAM:  89.0/96GB Temp:  33°C Freq:2.92GHz Pwr:118/600W
      Slowdown:95°C Shutdown:98°C MaxOp:93°C P-State:P1
      HW GSP:default v595.45.04
     Util : [▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬───────   78.0%]  Mem  : [▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬  89.0GB]























h:Help q:Exit ←→:Tabs ↑↓:Scroll PgUp/PgDn:Page d:Default u:Util g:GPU-Mem [Sort:Default]
```

After, the All tab:

```
 all-smi  cluster                                                                                           2 of 2 hosts up  ·  2026-09-25 23:34:51  ·  v0.26.3

 ACCELERATORS          GPU UTIL              UNIFIED MEMORY        VRAM                  HOST RAM              POWER                 TEMP MAX
 3                     1% avg                4.8 / 256 GiB         175 / 191 GiB         270 / 503 GiB         190 W                 42°C
 1 SoC · 2 dGPU        ━──────────────────   ━─────────────── 2%   ━━━━━━━━━━━━━━─ 92%   ━━━━━━━━─────── 54%   limit 1.20 kW         hottest device

  All  Users  Topology  ians-Mac-Studio  vllm
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
   DEVICE                            UTIL                                 MEMORY                                                   TEMP       POWER      CLOCK
 ● ians-Mac-Studio   macOS · Apple M5 Ultra · 36-core CPU  ·  CPU 6%  ·  RAM 4.8/256 GiB unified
   Apple M5 Ultra · 80-core GPU      ━─────────────────────────────   4%  ━───────────────────────────── 4.8/256 GiB    unified    42°C      0.03 W    600 MHz

 ● vllm   Linux · AMD EPYC 9275F · 48-core CPU  ·  CPU 5%  ·  RAM 270/503 GiB
   NVIDIA RTX PRO 6000 Blackwell #0  ──────────────────────────────   0%  ━━━━━━━━━━━━━━━━━━━━━━━━━━━─── 87.5/95.6 GiB  VRAM       32°C    93/600 W   2610 MHz
   NVIDIA RTX PRO 6000 Blackwell #1  ──────────────────────────────   0%  ━━━━━━━━━━━━━━━━━━━━━━━━━━━─── 87.5/95.6 GiB  VRAM       32°C    96/600 W   2610 MHz

 History ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────── last 5m 06s
 Apple M5 Ultra · 80-core GPU                          NVIDIA RTX PRO 6000 Blackwell #0                      NVIDIA RTX PRO 6000 Blackwell #1
 util                                   4% · peak 6%   util                                   0% · peak 0%   util                                   0% · peak 0%
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
 ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⢠⣤⣤⣤⣴⣤⣤⣤⣤⣤⣤⣤   ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀   ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
 unified mem                            2% · peak 3%   VRAM                                 92% · peak 92%   VRAM                                 91% · peak 91%
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⢠⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤                                          ⢠⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿                                          ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿                                          ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿                                          ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿                                          ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿                                          ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿                                          ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿
 ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀   ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿   ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿
 power                          0.03 W · peak 0.05 W   power                          93.1 W · peak 93.6 W   power                          96.5 W · peak 97.5 W
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
                                        ⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀                                          ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀                                          ⢠⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤
 ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⢠⣤⣤⣄⣤⣄⣀⣄⣀⣀⣠⣄   ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿   ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿


h:Help q:Exit ←→:Tabs ↑↓:Scroll PgUp/PgDn:Page x:Details d:Default u:Util g:GPU-Mem [Sort:Default]
```

The Consolidated tab with `--icculis` (the header above it is the same):

```
 Consolidated ────────────────────────────────────────────────────────────────────────────────────────────────── 3 accelerators on 2 hosts (2 up) as one system
HOST             DEVICE                               UTIL MEMORY                         POWER  TEMP     CLOCK  UTIL HISTORY            POWER HISTORY
ians-Mac-Studio  Apple M5 Ultra · 80-core GPU         3.8% 4.8/256.0 GiB unified         0.03 W  42°C   600 MHz             ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀            ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
                 └ ANE 0.00 W · CPU 0.00 W
vllm             NVIDIA RTX PRO 6000 Blackwell #0     0.0% 87.5/95.6 GiB VRAM        93.1/600 W  32°C  2610 MHz             ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀            ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
                 NVIDIA RTX PRO 6000 Blackwell #1     0.0% 87.5/95.6 GiB VRAM        96.5/600 W  32°C  2610 MHz             ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀            ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
TOTAL            3 accelerators                       1.3% 179.8/447.2 GiB 40%            190 W                             ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀            ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
                 unified 4.8/256.0 GiB, SoC GPU 0.03 W · VRAM 175.0/191.2 GiB, discrete GPUs 190 W

 Icculis · DeepSeek-V4.1-Flash · original weights ────────────────────────────────────────────────────────────────── engine ok · c75a0a2 · og-s4.4 · up 27m 30s
                  ╭ RTX box · vllm ────────────────────────────────╮         10GbE          ╭ M5 Ultra · ians-Mac-Studio ────────────────────╮
                  │ layers 0-19 · prefill + verify                 │ ───── 624 B/s ▶ ────── │ layers 20-39 · head · DSpark draft             │
                  │ ○ idle                                         │ ──── ◀ 21.2 KB/s ───── │ ○ idle  not serving                            │
                  ╰────────────────────────────────────────────────╯          idle          ╰────────────────────────────────────────────────╯
ENGINE           vllm :10051           sessions 0 · connections 0 · queue 0 · prefix cache 33% of 42 lookups · 4.9/64.0 GiB  busy ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀  queue ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
SWAP             ians-Mac-Studio :8080 no model loaded
LOCK             ians-Mac-Studio       ~/llm/locks/gpu.lock  free
LINK             ians-Mac-Studio       en0             ↓    302 B/s   ↑  21.2 KB/s                     ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀                   ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
LINK             vllm                  enp161s0f0np0   ↓  44.2 KB/s   ↑    624 B/s                     ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀                   ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
```

### The Icculis split

Icculis serves DeepSeek-V4.1-Flash (original weights) across the two
machines: the RTX box runs layers 0-19 for the whole prompt in PREFILL
and streams the state over the 10GbE link, then runs layers 0-19 of
every DECODE step's 1-5 verify rows; the Mac runs layers 20-39, the head
and the DSpark drafter for every step. The panel shows which half is
doing what, from what is measurable:

* box half: the engine `/health` GPU job. `prefill_chunk`, `vision`,
  `restore` or `snapshot` is PREFILL, timed from the first poll that saw
  it, with the current chunk's time; `step` is DECODE, and so is an open
  session while the link carries traffic (a step takes milliseconds and
  usually falls between polls);
* Mac half: the Mac exporter's JSON probes of the ds41 supervisor on
  port 10001: DECODE when the og worker's step counter moves, with
  `≈tok/s` estimated as steps per second times the worker's lifetime
  tokens per step (`total_completion_tokens / steps`); waiting when a
  request is in flight while the box prefills; "not serving" (with the
  GPU load and `gpu.lock` holder when the GPU is busy anyway) when the
  supervisor is not running;
* link: both exporters' interface rates, per direction, labeled as the
  prefill state or decode steps.

Live one-line strips from the All tab while another job drove the split
through the box engine (ds41 itself was not loaded, so the Mac half shows
its GPU load and lock holder instead of the ds41 step rate):

```
 Icculis  DeepSeek-V4.1-Flash · original weights  ·  no model loaded
 RTX box  ● PREFILL ▶ M5 Ultra  ● BUSY
 RTX box L0-19  ● DECODE  verify step   ── 15.5 MB/s ◀▶ ──   M5 Ultra L20-39 + head + draft  ● BUSY  not serving · GPU 72% · lock: python (pid 19648)
 RTX box L0-19  ○ idle   ── 41.1 KB/s ▶ ──   M5 Ultra L20-39 + head + draft  ○ idle  not serving
```

With ds41 serving, the Mac half reads `● DECODE  ≈63 tok/s · 31 steps/s`
while decoding and `○ waiting  for the prefill state` while the box
prefills, and the link column of the diagram reads `prefill state ▶` or
`◀ decode steps ▶`.

## Deployment on this pair

| Machine | Service | Listens on | Probes |
|---|---|---|---|
| RTX box (`vllm`) | systemd user unit `all-smi-api` (`~/.config/systemd/user/all-smi-api.service`, from `packaging/consolidated/all-smi-api.service`; linger is on) | `10.10.10.1:9090`, `127.0.0.1:9090` | `enp161s0f0np0` |
| Mac Studio | launchd user agent `com.tacos8me.all-smi-api` (`~/Library/LaunchAgents/`, from `packaging/consolidated/com.tacos8me.all-smi-api.plist`), bootstrapped into `user/501` | `10.10.10.2:9090`, `127.0.0.1:9090` | `en0`, `~/llm/locks/gpu.lock`, JSON probes `sup`/`og`/`omlx` on `127.0.0.1:10001` |

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
- The Mac's JSON probes point at port 10001, the port llama-swap gives
  the ds41 supervisor; if the llama-swap config changes that port, update
  the plist. The decode `≈tok/s` is an estimate (step rate times the
  worker's lifetime tokens per step); the exact per-request rate is only
  in the worker's log.
- "Held for" on the LOCK row uses the holder's process start when the
  holder predates the exporter; that is exact for servers that take the lock
  at startup and an upper bound otherwise.
