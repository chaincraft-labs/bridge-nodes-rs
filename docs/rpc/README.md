# RPC Server for Key Distribution

This module implements a JSON-RPC server that provides a communication interface for a peer-to-peer network. Built with `jsonrpsee`, it enables seamless interaction with relay nodes in a distributed system.

## Features

- Asynchronous RPC server implementation, over HTTP only (no WebSocket)
- Key distribution across peer nodes
- Peer ID validation and parsing
- Commands handed to the relayer's run loop over a channel, so an idle relayer
  answers at once
- Comprehensive error handling and logging

## Bind address

The server listens on TCP port 8545 of `127.0.0.1` by default: only the
relayer's own machine reaches it. The RPC administers the network and has no
authentication, so expose it on purpose only, with `--rpc-address <IP>`:

```sh
./validator_node --relayer --rpc-address 172.21.0.2
```

The port stays 8545. `--rpc-address` does not change the P2P listen address
(`--listen-address`).

The address must be a specific one: `0.0.0.0` and `::` are refused at start.
The server answers only a request whose `Host` header is `localhost`,
`127.0.0.1` or `[::1]`, or the address it is bound to, each with port 8545.
Any other `Host` gets HTTP 403. This stops a web page on the relayer's
machine from reaching the RPC through a DNS name rebound to it. Call the RPC
by IP (or `localhost`), not by a DNS name.

The server speaks HTTP only. A WebSocket upgrade is refused: browsers apply
no CORS to WebSocket, so any web page could otherwise open one to the RPC.

## API Methods

### distribute_keys

Initiates the key distribution process to a list of peer nodes.

#### Parameters

`peer_ids`: Vector of peer ID strings

#### Returns

- Success: String confirmation message
- Error: RpcError enum variant with details. A peer that is not connected
  stops the call with `Distribution failed: Peer error: Peer <id> not
  connected. ...`; peers before it in the list have already been sent their
  request.

### authorize_node

Authorizes peers: the relayer accepts their connections from then on.

#### Parameters

`peer_ids`: Vector of peer ID strings. It must not be empty, and every ID
must be valid: one invalid ID rejects the whole call (`Invalid peer ID: <id>`)
and nothing is applied. `distribute_keys` drops invalid IDs; these methods do
not.

#### Returns

The authorized peers after the call, as for `authorized_nodes`. Authorizing
peers that are all authorized already changes nothing: the answer is the
current state, and no new tree is signed.

### revoke_node

Revokes peers: the relayer closes their connections and refuses new ones.

#### Parameters

`peer_ids`: as for `authorize_node`.

#### Returns

The authorized peers after the call, as for `authorized_nodes`. Revoking
peers none of which is authorized changes nothing and signs no new tree.

### authorized_nodes

Lists the authorized peers. No parameter.

#### Returns

```json
{
  "peer_ids": ["12D3KooWDgCeJ15KaPzEEhV8XJd1xoN7hYBBVLdx8tBPiDgA5Lfo"],
  "root": "96d9d8012190ad661272a614b24671919d1ac5e11235d34f6300f12c2d04bf7c",
  "timestamp": 1793343404
}
```

- `peer_ids`: the authorized peer IDs, sorted.
- `root`: the root, in hex, of the Merkle tree over them that the relayer
  signed with its key.
- `timestamp`: the timestamp of that tree, in seconds. Every tree the relayer
  signs carries a timestamp strictly greater than the previous one, across
  restarts too.

On day one no peer is authorized: the relayer denies every connection until
the administrator authorizes one.

### Where the list is kept

Outside `--local-test`, the relayer stores its signed tree in
`$HOME/.chaincraft/authorized_peers.json` (directory mode 0700, file mode
0600) and loads it at start. A missing file means no authorized peer. A file
that does not parse, is not signed by the relayer's key, or whose peers do not
match its signed root stops the relayer with an error. Under `--local-test`
the file is neither read nor written.

## RPC request for Key Distribution

### Start the relayer node

```sh
# relayer ip  : 172.21.0.2
# relayer port: 62649
# The RPC is called from another container, so it binds the relayer's bridge
# address instead of the default 127.0.0.1.
./validator_node --new-peer-id
# output
12D3KooWSLDSpVvxqdCurqbUXLMGBy3c6yKoW7aDeyNE6hxmvJZV

./validator_node --relayer --rpc-address 172.21.0.2
```

### Start the validator nodes

```sh
# node 1 (repeat for n nodes)
./validator_node --new-peer-id
# output
12D3KooWGVHAcxbm7LMsKn4rSv4EtahEAgqxe4kjXNgDCs7EZnFE

# on the relayer: authorize the node before it connects
curl -X POST -H "Content-Type: application/json" --data '{
  "jsonrpc": "2.0",
  "method": "authorize_node",
  "params": { "peer_ids": ["12D3KooWGVHAcxbm7LMsKn4rSv4EtahEAgqxe4kjXNgDCs7EZnFE"] },
  "id": 1
}' http://172.21.0.2:8545

./validator_node --validator --bootstrap-address 172.21.0.2 --bootstrap-port 62649 --bootstrap-peer-id 12D3KooWSLDSpVvxqdCurqbUXLMGBy3c6yKoW7aDeyNE6hxmvJZV
```

### Request through rpc api on relayer node

```sh
curl -X POST -H "Content-Type: application/json" --data '{
  "jsonrpc": "2.0",
  "method": "distribute_keys",
  "params": {
    "peer_ids": [
      "12D3KooWLzFY5SmasJEWKCsGNFnF96R1xjZZGyh6PYgvaon2nFZG",
      "12D3KooWAMiFM8yG2WGN8hSAdsSu8Eorc3QTyuDLzkTGv2Gf52kG",
      "12D3KooWT3M6H2rAqVWdQi6sWKAJwQh5jpTewyQd3WrUcYP39wA5",
      "12D3KooWLzTQnPnc7xJaJWX1xtUGLgNF7TRYtnMUjXG8z4QELzvq"
    ]
  },
  "id": 1
}' http://172.21.0.2:8545
```
