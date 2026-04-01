# NTP Requirement for Commputer Nodes

## Why NTP Is Required

Commputer uses **round-robin leader election** with a 6-second view change interval and a 5-second clock skew tolerance window.

When your system clock drifts, the following problems occur:

1. **You reject valid blocks** — Your node calls `is_valid_leader` which checks `seconds_waiting` against the current system time. If your clock is slow, you think insufficient time has passed and reject legitimate fallback leaders.

2. **You produce blocks at the wrong time** — If your clock is fast, you may try to produce blocks before your assigned slot, causing your peers to reject them.

3. **You get marked Stale** — The `NodeStateMachine` compares received block heights against `network_height`. If your timestamps are off, block timestamps may fail validation, stalling your sync.

4. **Grace period drain miscalculation** — Grace period is tracked using wall-clock time. A drifted clock can cause incorrect grace balance calculations.

**The maximum tolerated clock skew is 5 seconds.** Beyond that, blocks may be rejected for having timestamps before their parent, and leader election timing will be incorrect.

---

## How to Enable NTP on Linux

### Using systemd-timesyncd (recommended, built-in)

```bash
# Enable and start the service
sudo systemctl enable --now systemd-timesyncd

# Verify it is running and synchronized
timedatectl status
```

Expected output when healthy:
```
               Local time: Sat 2024-01-01 12:00:00 UTC
           Universal time: Sat 2024-01-01 12:00:00 UTC
                 RTC time: Sat 2024-01-01 12:00:00
                Time zone: UTC (UTC, +0000)
System clock synchronized: yes
              NTP service: active
          RTC in local TZ: no
```

The line `System clock synchronized: yes` confirms NTP is working.

### Using chrony (alternative, recommended for VPS/cloud)

```bash
# Debian/Ubuntu
sudo apt install chrony
sudo systemctl enable --now chronyd

# Arch Linux
sudo pacman -S chrony
sudo systemctl enable --now chronyd

# Fedora/RHEL
sudo dnf install chrony
sudo systemctl enable --now chronyd
```

Check sync status:
```bash
chronyc tracking
```

### Using ntpd (legacy)

```bash
# Debian/Ubuntu
sudo apt install ntp
sudo systemctl enable --now ntp
```

---

## Troubleshooting

### "NTP sync" check shows WARN at startup

The `config_validator` checks for:
- `/run/systemd/timesync/synchronized`
- `/var/run/ntpd.pid`
- `/var/run/chrony/chrony.pid`
- `timedatectl show NTPSynchronized=yes`

If none of these are present, you will see:

```
[WARN] NTP sync: Cannot verify NTP synchronization status →
  Enable NTP: systemctl enable --now systemd-timesyncd (or ntpd, chrony)
```

This is a **warning**, not an error. The node will start. But you should fix it.

### Clock is synchronized but skew is high

Check current offset:
```bash
# systemd-timesyncd
timedatectl show-timesync --no-pager

# chrony
chronyc tracking | grep "System time"

# ntpq
ntpq -p
```

If offset is persistently >1 second, your NTP server may be unreachable or misconfigured.

Common fix:
```bash
# Force immediate sync
sudo systemctl restart systemd-timesyncd
# or
sudo chronyc makestep
```

### Firewall blocking NTP (port 123/UDP)

NTP uses UDP port 123. If your firewall blocks outbound UDP 123, NTP cannot synchronize.

```bash
# Allow outbound NTP
sudo ufw allow out 123/udp

# Or with iptables
sudo iptables -A OUTPUT -p udp --dport 123 -j ACCEPT
```

### Running on a system without internet access

For air-gapped or LAN-only deployments, set up a local NTP server:

1. Designate one machine as the NTP server (it needs internet or a GPS clock)
2. Install and configure `chrony` or `ntpd` on that machine to serve NTP
3. Configure all Commputer nodes to use the local NTP server:

```bash
# In /etc/systemd/timesyncd.conf
[Time]
NTP=192.168.1.1  # your local NTP server
```

---

## Summary

| Service | Status Check | Enable Command |
|---------|-------------|----------------|
| systemd-timesyncd | `timedatectl status` | `systemctl enable --now systemd-timesyncd` |
| chrony | `chronyc tracking` | `systemctl enable --now chronyd` |
| ntpd | `ntpq -p` | `systemctl enable --now ntp` |

**Minimum requirement:** Clock offset < 3 seconds from network time at all times.
**Recommended:** Clock offset < 500ms (most NTP implementations achieve <50ms on a stable internet connection).
