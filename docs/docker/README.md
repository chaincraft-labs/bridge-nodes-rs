# Run nodes with Docker

## Built the image

```sh
docker build -t libp2p-node:latest .
```

## Start the nodes

```sh
docker compose -f DockerCompose.yml u
```

## Connect to the nodes

### Relayer

```sh
docker exec -it node_kdm_bootstrap /bin/bash
```

### Validators

```sh
docker exec -it node_kdm_client1 /bin/bash
docker exec -it node_kdm_client2 /bin/bash
...
docker exec -it node_kdm_client5 /bin/bash
```
