# seed-node-kit

Operator + founder toolkit for managing the Commputer L1 built-in seed node
list. The chain currently ships with `SEED_NODES = &[]` — a real testnet
needs 2-3 well-known seeds, each contributed by a trusted operator.

This kit contains three small CLI binaries and this README. Each `.rs` file
is intentionally standalone so the founder can move them individually.

| File | Binary name | Purpose |
| --- | --- | --- |
| `keygen.rs` | `commputer-keygen` | Generate a libp2p Ed25519 node keypair |
| `multiaddr_builder.rs` | `commputer-multiaddr-builder` | Compose a canonical multiaddr from peer-id + ip + port |
| `verify_multiaddr.rs` | `commputer-verify-multiaddr` | Parse + best-effort dial test for a multiaddr |

> All three use the same libp2p version the node already runs (`libp2p` 0.54
> → `libp2p-identity` 0.2). Keypairs are written in the protobuf encoding
> the node loader expects at
> `src/network/src/transport.rs:173` (`Keypair::from_protobuf_encoding`).

---

## Operator workflow

You are an operator who has volunteered to host a seed node. You will:

1. **Generate a keypair on the machine that will run the seed node.** This
   machine should be the one that will hold the long-lived public IP /
   DNS name. The peer ID is derived from the public key, so the keypair
   *is* the identity.

   ```bash
   commputer-keygen --out ~/.commputer/peer_id
   ```

   Output:

   ```
   Generated libp2p Ed25519 keypair
     peer id : 12D3KooWExample...
     key file: ~/.commputer/peer_id
     bytes   : 68
   ```

   The `--out` file is `chmod 0600` immediately. Do not run as root unless
   you also chown it to the user that will run the node. `keygen` creates
   the parent directory if it does not exist.

2. **The node loads its identity ONLY from `~/.commputer/peer_id`** (a fixed
   path; see `new_with_keypair_path` in `src/network/src/transport.rs:169`,
   reading the path from `config::peer_key_path()`). That is why step 1
   writes the key straight there. If you generated the key elsewhere, copy
   it into place BEFORE the first node start:

   ```bash
   mkdir -p ~/.commputer && cp <your-key> ~/.commputer/peer_id && chmod 600 ~/.commputer/peer_id
   ```

   If you skip this, the node generates a DIFFERENT random identity on first
   boot (transport.rs:177) and the peer ID you registered with the founder
   will not match your running node — seed connections to you will fail.

3. **Build your multiaddr** using the peer ID printed above plus the
   public IP/DNS and port you intend to expose:

   ```bash
   commputer-multiaddr-builder \
     --peer-id 12D3KooWExample... \
     --ip 203.0.113.10 \
     --port 9000
   # → /ip4/203.0.113.10/tcp/9000/p2p/12D3KooWExample...
   ```

   Use `--proto quic` if you want to advertise the QUIC-v1 transport
   instead of TCP.

4. **Self-verify** before you send it out:

   ```bash
   commputer-verify-multiaddr "/ip4/203.0.113.10/tcp/9000/p2p/12D3KooW..."
   ```

   Confirm `parse: ok`, the peer id matches what `keygen` printed, and
   `reachable: true`. If it's not reachable, fix firewall / NAT before
   handing the address to the founder.

5. **Send the multiaddr string to the founder** through whatever
   out-of-band channel you've already established. The founder will
   include it in the next release. You do **not** send the key file. You
   never send the key file. The key file stays on your machine.

---

## Founder workflow

When an operator hands you a multiaddr:

1. Run `commputer-verify-multiaddr "<addr>"` from your own network as a
   sanity check. Parse must succeed; reachability is informational
   (operator may not be live yet at the moment you check).

2. Open `src/network/src/transport.rs` and locate the constant at
   **`src/network/src/transport.rs:316`**:

   ```rust
   /// Founder-operated seed nodes. Replace with real addresses at launch.
   pub const SEED_NODES: &[&str] = &[
       // Format: /ip4/<IP>/tcp/<PORT>/p2p/<PEER_ID>
       // Or QUIC: /ip4/<IP>/udp/<PORT>/quic-v1/p2p/<PEER_ID>
   ];
   ```

   Paste the new multiaddr as a string element. Keep the comment header
   intact so the next maintainer knows the format.

3. Tag a release. Operators who upgrade now ship with the new seed node
   baked in.

4. After the release ships, clean up by deleting any short-lived
   bootstrap addresses that were only meant to ride out the cold start.

> The kit deliberately does **not** modify `SEED_NODES` itself. That is a
> founder-only step (the constant lives in a non-protected file but is
> the integrity hinge for chain bootstrap, so changes go through review).

---

## Security: protecting the private key

The private key file is the seed node's identity. Anyone with that file
can impersonate the seed and have other nodes treat them as a trusted
bootstrap. Treat it like a TLS server private key.

Operator hygiene checklist:

- **Permissions:** `keygen` writes `0600` automatically on Unix. If you
  copy the file, preserve permissions (`scp -p` / `cp --preserve=mode`).
- **Owner:** the file should be owned by the user that runs the node
  process. Not root unless your service runs as root.
- **No backups in cleartext.** If you back it up, encrypt the backup
  (e.g. `age` / `gpg`). Treat the backup as if it were the original.
- **No source control.** Add `seed_key.bin` (or whatever you named it)
  to `.gitignore` on every machine. Never commit it. Never paste it in
  chat or a ticket.
- **Loss = compromise.** If you suspect the key file leaked (lost laptop,
  shared dev box, accidentally tarred into a public release), assume
  compromise and rotate.

### Rotation procedure

1. Generate a new keypair on the seed machine:

   ```bash
   commputer-keygen --out /etc/commputer/seed_key.new.bin
   ```

2. Build the new multiaddr (`commputer-multiaddr-builder`).

3. Send the **new** multiaddr to the founder. The founder appends the
   new entry to `SEED_NODES` in `src/network/src/transport.rs:316` and
   tags an interim release.

4. Once the new release is the dominant version on the network, restart
   your node to pick up `seed_key.new.bin` (atomically replace the path
   it loads, or update config to point at the new file).

5. The founder cuts a final release that **removes** the old multiaddr
   from `SEED_NODES`.

6. Delete `seed_key.bin` (the old one) from disk and any backups.

Rotation is rare but should be exercised at least once on testnet so
the muscle memory exists for mainnet.

---

## Genesis distribution

Seed nodes are necessary but not sufficient for a peer to join the chain
— every node also needs the same `genesis.json`. The canonical copy lives
at the repo root:

- `genesis.json` (workspace root)
- `src/genesis.json` (build-time sibling)

Operators should fetch this from the same release artifact that contains
the binary, or from the official Git tag, **never** from a side channel.
A mismatched genesis manifests as identify-handshake rejections at
`src/network/src/transport.rs` (the genesis hash is included in
`identify.agent_version` for exactly this reason — see comments around
`new_with_keypair_path`).

If you change `genesis.json`, you've forked the chain. That is a founder
decision, not an operator decision.

---

## Wiring the kit into the workspace (founder, one-time)

Two equally fine options. Pick one:

**Option A — separate crate (recommended; smallest blast radius):**

1. Create `src/seed-node-kit/` with its own `Cargo.toml`:
   ```toml
   [package]
   name        = "seed-node-kit"
   version.workspace    = true
   edition.workspace    = true
   license.workspace    = true
   repository.workspace = true
   description = "Operator toolkit for managing Commputer seed nodes"

   [dependencies]
   libp2p          = { workspace = true }
   libp2p-identity = { version = "0.2", features = ["ed25519", "peerid", "rand"] }
   clap            = { workspace = true }
   anyhow          = { workspace = true }
   tokio           = { workspace = true }

   [[bin]]
   name = "commputer-keygen"
   path = "src/keygen.rs"

   [[bin]]
   name = "commputer-multiaddr-builder"
   path = "src/multiaddr_builder.rs"

   [[bin]]
   name = "commputer-verify-multiaddr"
   path = "src/verify_multiaddr.rs"
   ```

2. Move the three `.rs` files from `src/staging/seed_node_kit/` to
   `src/seed-node-kit/src/`.

3. Add `"seed-node-kit"` to `[workspace] members` in `src/Cargo.toml`.

**Option B — fold into commputer-network:**

1. Add `clap = { workspace = true }` and `anyhow = { workspace = true }` to
   `src/network/Cargo.toml`.
2. Move the three `.rs` files into `src/network/src/bin/`.
3. Switch the `use libp2p_identity::...` and `use libp2p::...` lines to
   the existing `libp2p::identity` re-export already used elsewhere in
   the crate.

Either way, none of the existing `commputer-network` source code needs
to change.
