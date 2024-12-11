# Commun commands for nodes

## Create a new peer id

Create a peer id on both relayer and validator node

```sh
./validator_node --new-peer-id

# output
Peer ID : 12D3KooWKpXWhPYWxAmWhHiEWF2GDew2tAF1hLdnjBwjcTM8HACx
```

## Start a relayer node

```sh
./validator_node --relayer

# output
2024-12-09T21:22:52.635335Z  INFO validator_node::node::relayer: 🌟 Relayer node
2024-12-09T21:22:52.638474Z  INFO libp2p_swarm: local_peer_id=12D3KooWKpXWhPYWxAmWhHiEWF2GDew2tAF1hLdnjBwjcTM8HACx
2024-12-09T21:22:52.639659Z  INFO validator_node::node::relayer: Subscribed to topic: custom_events
2024-12-09T21:22:52.639851Z  INFO validator_node::rpc::server: RPC server started on 127.0.0.1:8545
2024-12-09T21:22:52.745567Z  INFO validator_node::node::common: Listening on /ip4/127.0.0.1/udp/62649/quic-v1
2024-12-09T21:22:52.745667Z  INFO validator_node::node::common: Listening on /ip4/172.21.0.2/udp/62649/quic-v1
2024-12-09T21:22:53.272138Z  WARN libp2p_kad::behaviour: Failed to trigger bootstrap: No known peers.
```

## Start a validator node

```sh
./validator_node --validator \
    --bootstrap-peer-id 12D3KooWKpXWhPYWxAmWhHiEWF2GDew2tAF1hLdnjBwjcTM8HACx \
    --bootstrap-address 172.21.0.2 --bootstrap-port 62649
```
