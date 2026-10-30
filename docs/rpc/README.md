# RPC Server for Key Distribution

This module implements a JSON-RPC server that provides a communication interface for a peer-to-peer network. Built with `jsonrpsee`, it enables seamless interaction with relay nodes in a distributed system.

## Features

- Asynchronous RPC server implementation
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
