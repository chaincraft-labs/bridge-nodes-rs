# RPC Server for Key Distribution

This module implements a JSON-RPC server that provides a communication interface for a peer-to-peer network. Built with `jsonrpsee`, it enables seamless interaction with relay nodes in a distributed system.

## Features

- Asynchronous RPC server implementation
- Key distribution across peer nodes
- Peer ID validation and parsing
- Thread-safe relayer node access using tokio's Mutex
- Comprehensive error handling and logging

## API Methods

### distribute_keys

Initiates the key distribution process to a list of peer nodes.

#### Parameters

`peer_ids`: Vector of peer ID strings

#### Returns

- Success: String confirmation message
- Error: RpcError enum variant with details

## RPC request for Key Distribution

### Start the relayer node

```sh
# relayer ip  : 172.21.0.2
# relayer port: 62649
./validator_node --new-peer-id
# output
12D3KooWSLDSpVvxqdCurqbUXLMGBy3c6yKoW7aDeyNE6hxmvJZV

./validator_node --relayer
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
