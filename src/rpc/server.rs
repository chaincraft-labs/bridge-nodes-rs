use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
    server::{middleware::http::HostFilterLayer, ServerBuilder, ServerConfig, ServerHandle},
};
use libp2p::PeerId;
use std::net::SocketAddr;
use std::str::FromStr;
use tokio::sync::oneshot;
use tracing::info;

use super::types::RpcError;
use crate::node::authorized::AuthorizedNodes;
use crate::node::relayer::{CommandSender, RelayerCommand};
use crate::types::NodeError;

#[rpc(server, client)]
pub trait RpcApi {
    #[method(name = "distribute_keys")]
    async fn distribute_keys(&self, peer_ids: Vec<String>) -> RpcResult<String>;

    /// Authorizes the peers. Answers with the authorized peers after the call.
    #[method(name = "authorize_node")]
    async fn authorize_node(&self, peer_ids: Vec<String>) -> RpcResult<AuthorizedNodes>;

    /// Revokes the peers and closes their connections. Answers with the
    /// authorized peers after the call.
    #[method(name = "revoke_node")]
    async fn revoke_node(&self, peer_ids: Vec<String>) -> RpcResult<AuthorizedNodes>;

    /// The authorized peers.
    #[method(name = "authorized_nodes")]
    async fn authorized_nodes(&self) -> RpcResult<AuthorizedNodes>;
}

/// Serves the RPC by sending commands to the relayer's run loop: it holds no
/// node state of its own.
pub struct RpcServerImpl {
    commands: CommandSender,
}

/// Parses every peer ID, or none: one invalid ID rejects the whole list, and
/// so does an empty one.
fn parse_all(peer_ids: &[String]) -> Result<Vec<PeerId>, RpcError> {
    if peer_ids.is_empty() {
        return Err(RpcError::NoPeers);
    }
    peer_ids
        .iter()
        .map(|id| PeerId::from_str(id).map_err(|_| RpcError::InvalidPeerId(id.clone())))
        .collect()
}

impl RpcServerImpl {
    pub fn new(commands: CommandSender) -> Self {
        Self { commands }
    }

    // Sends the command `make` builds around a reply channel, and waits for
    // the answer.
    async fn ask<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> RelayerCommand,
    ) -> Result<T, RpcError> {
        let (reply, answer) = oneshot::channel();
        if self.commands.send(make(reply)).await.is_err() {
            return Err(RpcError::RelayerStopped);
        }
        answer.await.map_err(|_| RpcError::RelayerStopped)
    }

    async fn change(
        &self,
        peer_ids: Vec<String>,
        make: impl FnOnce(
            Vec<PeerId>,
            oneshot::Sender<Result<AuthorizedNodes, NodeError>>,
        ) -> RelayerCommand,
    ) -> RpcResult<AuthorizedNodes> {
        let peers = parse_all(&peer_ids)?;
        self.ask(|reply| make(peers, reply))
            .await?
            .map_err(|e| RpcError::AuthorizationFailed(e.to_string()).into())
    }
}

#[async_trait]
impl RpcApiServer for RpcServerImpl {
    async fn distribute_keys(&self, peer_ids: Vec<String>) -> RpcResult<String> {
        let peers = peer_ids
            .iter()
            .filter_map(|id| PeerId::from_str(id).ok())
            .collect::<Vec<_>>();

        if peers.is_empty() {
            return Err(RpcError::NoPeers.into());
        }

        match self
            .ask(|reply| RelayerCommand::DistributeKeys { peers, reply })
            .await?
        {
            Ok(()) => Ok("Key distribution started successfully".to_string()),
            Err(e) => Err(RpcError::DistributionFailed(e.to_string()).into()),
        }
    }

    async fn authorize_node(&self, peer_ids: Vec<String>) -> RpcResult<AuthorizedNodes> {
        self.change(peer_ids, |peers, reply| RelayerCommand::AuthorizeNodes {
            peers,
            reply,
        })
        .await
    }

    async fn revoke_node(&self, peer_ids: Vec<String>) -> RpcResult<AuthorizedNodes> {
        self.change(peer_ids, |peers, reply| RelayerCommand::RevokeNodes {
            peers,
            reply,
        })
        .await
    }

    async fn authorized_nodes(&self) -> RpcResult<AuthorizedNodes> {
        Ok(self
            .ask(|reply| RelayerCommand::AuthorizedNodes { reply })
            .await?)
    }
}

/// The `Host` headers the server answers: the loopback names and the address
/// it is bound to, each on its port. A page that reaches the server through a
/// DNS name rebound to this machine sends that name, and is refused.
fn allowed_hosts(bound: SocketAddr) -> Vec<String> {
    let port = bound.port();
    let mut hosts = vec![
        format!("localhost:{}", port),
        format!("127.0.0.1:{}", port),
        format!("[::1]:{}", port),
    ];
    let own = bound.to_string();
    if !hosts.contains(&own) {
        hosts.push(own);
    }
    hosts
}

/// Starts the JSON-RPC server, over HTTP only, on `addr` and returns its
/// handle, which stops the server when dropped, and the address it bound. An unspecified address
/// (`0.0.0.0`, `::`) is refused: the server answers only requests addressed
/// to the address it is bound to.
pub async fn start_rpc_server(
    commands: CommandSender,
    addr: SocketAddr,
) -> Result<(ServerHandle, SocketAddr), NodeError> {
    let failed = |e: &dyn std::fmt::Display| {
        NodeError::ConfigError(format!("Failed to build RPC server: {}", e))
    };
    if addr.ip().is_unspecified() {
        return Err(failed(&format!(
            "{} is not a specific address; give the IP clients call",
            addr.ip()
        )));
    }

    let listener = std::net::TcpListener::bind(addr).map_err(|e| failed(&e))?;
    listener.set_nonblocking(true).map_err(|e| failed(&e))?;
    let bound = listener.local_addr().map_err(|e| failed(&e))?;
    let host_filter = HostFilterLayer::new(allowed_hosts(bound)).map_err(|e| failed(&e))?;
    let server = ServerBuilder::default()
        // No WebSocket: browsers apply no CORS to it, so any web page could
        // open one to the loopback RPC and call it.
        .set_config(ServerConfig::builder().http_only().build())
        .set_http_middleware(tower::ServiceBuilder::new().layer(host_filter))
        .build_from_tcp(listener)
        .map_err(|e| failed(&e))?;

    let handle = server.start(RpcServerImpl::new(commands).into_rpc());
    info!("RPC server started on {}", bound);
    Ok((handle, bound))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::peer_id::generate_keypair;
    use jsonrpsee::http_client::HttpClientBuilder;
    use jsonrpsee::types::ErrorObjectOwned;
    use tokio::sync::mpsc;

    fn random_peer() -> PeerId {
        generate_keypair(None).public().to_peer_id()
    }

    // A stand-in for the relayer's run loop: answers every command with
    // `answer` and hands the peers it was given to the test.
    fn fake_relayer(
        answer: fn() -> Result<(), NodeError>,
    ) -> (CommandSender, mpsc::UnboundedReceiver<Vec<PeerId>>) {
        let (commands_tx, mut commands_rx) = mpsc::channel(8);
        let (seen_tx, seen_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(RelayerCommand::DistributeKeys { peers, reply }) =
                commands_rx.recv().await
            {
                seen_tx.send(peers).unwrap();
                let _ = reply.send(answer());
            }
        });
        (commands_tx, seen_rx)
    }

    #[tokio::test]
    async fn test_distribute_keys_sends_the_valid_ids_to_the_relayer() {
        let (commands, mut seen) = fake_relayer(|| Ok(()));
        let rpc = RpcServerImpl::new(commands);
        let (a, b) = (random_peer(), random_peer());

        let result = rpc
            .distribute_keys(vec![
                a.to_string(),
                "not-a-peer-id".to_string(),
                b.to_string(),
            ])
            .await;

        assert_eq!(result.unwrap(), "Key distribution started successfully");
        assert_eq!(seen.recv().await.unwrap(), vec![a, b]);
    }

    #[tokio::test]
    async fn test_distribute_keys_without_a_valid_id_reaches_no_relayer() {
        let (commands, mut seen) = fake_relayer(|| Ok(()));
        let rpc = RpcServerImpl::new(commands);

        assert_error(rpc.distribute_keys(vec![]).await, "No peers provided");
        assert_error(
            rpc.distribute_keys(vec!["not-a-peer-id".to_string()]).await,
            "No peers provided",
        );
        drop(rpc);
        assert!(seen.recv().await.is_none());
    }

    #[tokio::test]
    async fn test_distribute_keys_reports_the_relayer_error() {
        let (commands, _seen) = fake_relayer(|| {
            Err(NodeError::PeerError(
                "Peer X not connected. Ensure peer is connected before distributing keys"
                    .to_string(),
            ))
        });
        let rpc = RpcServerImpl::new(commands);

        assert_error(
            rpc.distribute_keys(vec![random_peer().to_string()]).await,
            "Distribution failed: Peer error: Peer X not connected. \
             Ensure peer is connected before distributing keys",
        );
    }

    #[tokio::test]
    async fn test_distribute_keys_with_the_relayer_gone_is_an_error() {
        let (commands, receiver) = mpsc::channel(8);
        drop(receiver);
        let rpc = RpcServerImpl::new(commands);

        assert_error(
            rpc.distribute_keys(vec![random_peer().to_string()]).await,
            "The relayer is not running",
        );
    }

    #[tokio::test]
    async fn test_distribute_keys_with_no_answer_is_an_error() {
        let (commands, mut receiver) = mpsc::channel(8);
        tokio::spawn(async move {
            // Takes the command and drops its reply channel unanswered.
            while receiver.recv().await.is_some() {}
        });
        let rpc = RpcServerImpl::new(commands);

        assert_error(
            rpc.distribute_keys(vec![random_peer().to_string()]).await,
            "The relayer is not running",
        );
    }

    #[tokio::test]
    async fn test_server_serves_distribute_keys_over_http() {
        let (commands, mut seen) = fake_relayer(|| Ok(()));
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (_handle, bound) = start_rpc_server(commands, addr).await.unwrap();
        assert_eq!(bound.ip(), addr.ip());
        let client = HttpClientBuilder::default()
            .build(format!("http://{}", bound))
            .unwrap();
        let peer = random_peer();

        let answer = RpcApiClient::distribute_keys(&client, vec![peer.to_string()])
            .await
            .unwrap();

        assert_eq!(answer, "Key distribution started successfully");
        assert_eq!(seen.recv().await.unwrap(), vec![peer]);
    }

    #[tokio::test]
    async fn test_server_that_cannot_bind_is_an_error() {
        let (commands, _seen) = fake_relayer(|| Ok(()));
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = taken.local_addr().unwrap();

        match start_rpc_server(commands, addr).await {
            Err(NodeError::ConfigError(message)) => {
                assert!(message.starts_with("Failed to build RPC server: "))
            }
            Err(other) => panic!("expected ConfigError, got {:?}", other),
            Ok(_) => panic!("bound a port that is taken"),
        }
    }

    #[derive(Debug, PartialEq)]
    enum Asked {
        Authorize(Vec<PeerId>),
        Revoke(Vec<PeerId>),
        List,
    }

    fn sample_state() -> AuthorizedNodes {
        AuthorizedNodes {
            peer_ids: vec![random_peer().to_string()],
            root: "ab".repeat(32),
            timestamp: 42,
        }
    }

    // A stand-in for the relayer's run loop for the authorization commands:
    // answers with `answer` (or its state, for the list) and hands what it
    // was asked to the test.
    fn fake_authority(
        answer: fn() -> Result<AuthorizedNodes, NodeError>,
        state: AuthorizedNodes,
    ) -> (CommandSender, mpsc::UnboundedReceiver<Asked>) {
        let (commands_tx, mut commands_rx) = mpsc::channel(8);
        let (seen_tx, seen_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(command) = commands_rx.recv().await {
                match command {
                    RelayerCommand::AuthorizeNodes { peers, reply } => {
                        seen_tx.send(Asked::Authorize(peers)).unwrap();
                        let _ = reply.send(answer());
                    }
                    RelayerCommand::RevokeNodes { peers, reply } => {
                        seen_tx.send(Asked::Revoke(peers)).unwrap();
                        let _ = reply.send(answer());
                    }
                    RelayerCommand::AuthorizedNodes { reply } => {
                        seen_tx.send(Asked::List).unwrap();
                        let _ = reply.send(state.clone());
                    }
                    RelayerCommand::DistributeKeys { .. } => panic!("not asked"),
                }
            }
        });
        (commands_tx, seen_rx)
    }

    fn assert_error<T: std::fmt::Debug>(result: RpcResult<T>, message: &str) {
        let error: ErrorObjectOwned = result.unwrap_err();
        assert_eq!(error.code(), 1);
        assert_eq!(error.message(), message);
    }

    #[tokio::test]
    async fn test_authorize_node_sends_every_id_and_returns_the_answer() {
        let state = sample_state();
        let (commands, mut seen) = fake_authority(
            || {
                Ok(AuthorizedNodes {
                    peer_ids: vec!["answered".to_string()],
                    root: String::new(),
                    timestamp: 7,
                })
            },
            state,
        );
        let rpc = RpcServerImpl::new(commands);
        let (a, b) = (random_peer(), random_peer());

        let answer = rpc
            .authorize_node(vec![a.to_string(), b.to_string()])
            .await
            .unwrap();

        assert_eq!(answer.peer_ids, vec!["answered".to_string()]);
        assert_eq!(answer.timestamp, 7);
        assert_eq!(seen.recv().await.unwrap(), Asked::Authorize(vec![a, b]));
    }

    #[tokio::test]
    async fn test_revoke_node_sends_every_id_and_returns_the_answer() {
        let (commands, mut seen) = fake_authority(|| Ok(sample_state()), sample_state());
        let rpc = RpcServerImpl::new(commands);
        let a = random_peer();

        assert!(rpc.revoke_node(vec![a.to_string()]).await.is_ok());
        assert_eq!(seen.recv().await.unwrap(), Asked::Revoke(vec![a]));
    }

    #[tokio::test]
    async fn test_authorized_nodes_returns_the_relayer_state() {
        let state = sample_state();
        let (commands, mut seen) = fake_authority(|| panic!("not asked"), state.clone());
        let rpc = RpcServerImpl::new(commands);

        assert_eq!(rpc.authorized_nodes().await.unwrap(), state);
        assert_eq!(seen.recv().await.unwrap(), Asked::List);
    }

    #[tokio::test]
    async fn test_one_invalid_id_rejects_the_whole_call() {
        let (commands, mut seen) = fake_authority(|| Ok(sample_state()), sample_state());
        let rpc = RpcServerImpl::new(commands);
        let valid = random_peer().to_string();

        assert_error(
            rpc.authorize_node(vec![valid.clone(), "not-a-peer-id".to_string()])
                .await,
            "Invalid peer ID: not-a-peer-id",
        );
        assert_error(
            rpc.revoke_node(vec!["12D3KooWX".to_string(), valid]).await,
            "Invalid peer ID: 12D3KooWX",
        );
        drop(rpc);
        assert!(seen.recv().await.is_none(), "nothing reached the relayer");
    }

    #[tokio::test]
    async fn test_an_empty_list_is_an_error_that_reaches_no_relayer() {
        let (commands, mut seen) = fake_authority(|| Ok(sample_state()), sample_state());
        let rpc = RpcServerImpl::new(commands);

        assert_error(rpc.authorize_node(vec![]).await, "No peers provided");
        assert_error(rpc.revoke_node(vec![]).await, "No peers provided");
        drop(rpc);
        assert!(seen.recv().await.is_none());
    }

    #[tokio::test]
    async fn test_authorization_reports_the_relayer_error() {
        let (commands, _seen) = fake_authority(
            || {
                Err(NodeError::AuthorizedPeers(
                    "Cannot write x: denied".to_string(),
                ))
            },
            sample_state(),
        );
        let rpc = RpcServerImpl::new(commands);
        let id = random_peer().to_string();

        let expected = "Authorization failed: Authorized peers: Cannot write x: denied";
        assert_error(rpc.authorize_node(vec![id.clone()]).await, expected);
        assert_error(rpc.revoke_node(vec![id]).await, expected);
    }

    #[tokio::test]
    async fn test_authorization_with_the_relayer_gone_is_an_error() {
        let (commands, receiver) = mpsc::channel(8);
        drop(receiver);
        let rpc = RpcServerImpl::new(commands);
        let id = random_peer().to_string();

        assert_error(
            rpc.authorize_node(vec![id.clone()]).await,
            "The relayer is not running",
        );
        assert_error(
            rpc.revoke_node(vec![id]).await,
            "The relayer is not running",
        );
        assert_error(rpc.authorized_nodes().await, "The relayer is not running");
    }

    #[tokio::test]
    async fn test_server_serves_the_authorization_methods_over_http() {
        let state = sample_state();
        let (commands, mut seen) = fake_authority(|| Ok(sample_state()), state.clone());
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (_handle, bound) = start_rpc_server(commands, addr).await.unwrap();
        let client = HttpClientBuilder::default()
            .build(format!("http://{}", bound))
            .unwrap();
        let peer = random_peer();

        RpcApiClient::authorize_node(&client, vec![peer.to_string()])
            .await
            .unwrap();
        RpcApiClient::revoke_node(&client, vec![peer.to_string()])
            .await
            .unwrap();
        let listed = RpcApiClient::authorized_nodes(&client).await.unwrap();

        assert_eq!(listed, state);
        assert_eq!(seen.recv().await.unwrap(), Asked::Authorize(vec![peer]));
        assert_eq!(seen.recv().await.unwrap(), Asked::Revoke(vec![peer]));
        assert_eq!(seen.recv().await.unwrap(), Asked::List);
    }

    // Posts an authorize_node call with the given Host header, over raw
    // HTTP, and returns the status line.
    async fn post_with_host(bound: SocketAddr, host: &str, peer: &PeerId) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let body = format!(
            r#"{{"jsonrpc":"2.0","method":"authorize_node","params":{{"peer_ids":["{}"]}},"id":1}}"#,
            peer
        );
        let request = format!(
            "POST / HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            host,
            body.len(),
            body
        );
        let mut stream = tokio::net::TcpStream::connect(bound).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response.lines().next().unwrap_or_default().to_string()
    }

    #[tokio::test]
    async fn test_a_rebound_host_is_refused_and_reaches_no_relayer() {
        let (commands, mut seen) = fake_authority(|| Ok(sample_state()), sample_state());
        let (handle, bound) = start_rpc_server(commands, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

        for host in [
            format!("rebind.attacker.example:{}", bound.port()),
            "rebind.attacker.example".to_string(),
            // The right name on another port is another origin.
            format!("localhost:{}", bound.port() + 1),
        ] {
            let status = post_with_host(bound, &host, &random_peer()).await;
            assert!(status.starts_with("HTTP/1.1 403"), "{}: {}", host, status);
        }

        handle.stop().unwrap();
        handle.stopped().await;
        assert!(seen.recv().await.is_none(), "nothing reached the relayer");
    }

    #[tokio::test]
    async fn test_the_loopback_names_are_served() {
        let (commands, mut seen) = fake_authority(|| Ok(sample_state()), sample_state());
        let (_handle, bound) = start_rpc_server(commands, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

        for name in ["localhost", "127.0.0.1", "[::1]"] {
            let peer = random_peer();
            let host = format!("{}:{}", name, bound.port());
            let status = post_with_host(bound, &host, &peer).await;
            assert!(status.starts_with("HTTP/1.1 200"), "{}: {}", host, status);
            assert_eq!(seen.recv().await.unwrap(), Asked::Authorize(vec![peer]));
        }
    }

    #[test]
    fn test_the_allowed_hosts_are_loopback_and_the_bound_address() {
        let bound: SocketAddr = "172.21.0.2:8545".parse().unwrap();
        assert_eq!(
            allowed_hosts(bound),
            vec![
                "localhost:8545",
                "127.0.0.1:8545",
                "[::1]:8545",
                "172.21.0.2:8545"
            ]
        );
        let loopback: SocketAddr = "127.0.0.1:8545".parse().unwrap();
        assert_eq!(
            allowed_hosts(loopback),
            vec!["localhost:8545", "127.0.0.1:8545", "[::1]:8545"]
        );
    }

    #[tokio::test]
    async fn test_an_unspecified_address_is_refused() {
        for addr in ["0.0.0.0:0", "[::]:0"] {
            let (commands, _seen) = fake_authority(|| Ok(sample_state()), sample_state());
            match start_rpc_server(commands, addr.parse().unwrap()).await {
                Err(NodeError::ConfigError(message)) => assert!(
                    message.starts_with("Failed to build RPC server: ")
                        && message.contains("is not a specific address"),
                    "{}",
                    message
                ),
                Err(other) => panic!("expected ConfigError, got {:?}", other),
                Ok(_) => panic!("{} was bound", addr),
            }
        }
    }

    #[tokio::test]
    async fn test_a_websocket_upgrade_is_refused() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (commands, mut seen) = fake_authority(|| Ok(sample_state()), sample_state());
        let (handle, bound) = start_rpc_server(commands, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        // What a page on any site sends: the Host is allowed, the Origin is
        // not, and browsers apply no CORS to WebSocket.
        let request = format!(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: https://evil.example\r\n\
             Upgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
            bound.port()
        );
        let mut stream = tokio::net::TcpStream::connect(bound).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = vec![0; 1024];
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read(&mut response),
        )
        .await
        .expect("the server answers the upgrade")
        .unwrap();
        let status = String::from_utf8_lossy(&response[..read])
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();

        assert!(status.starts_with("HTTP/1.1 "), "{}", status);
        assert!(!status.starts_with("HTTP/1.1 101"), "{}", status);
        drop(stream);
        handle.stop().unwrap();
        handle.stopped().await;
        assert!(seen.recv().await.is_none(), "nothing reached the relayer");
    }
}
