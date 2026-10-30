use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
    server::{ServerBuilder, ServerHandle},
};
use libp2p::PeerId;
use std::net::SocketAddr;
use std::str::FromStr;
use tokio::sync::oneshot;
use tracing::info;

use super::types::RpcError;
use crate::node::relayer::{CommandSender, RelayerCommand};
use crate::types::NodeError;

#[rpc(server, client)]
pub trait RpcApi {
    #[method(name = "distribute_keys")]
    async fn distribute_keys(&self, peer_ids: Vec<String>) -> RpcResult<String>;
}

/// Serves the RPC by sending commands to the relayer's run loop: it holds no
/// node state of its own.
pub struct RpcServerImpl {
    commands: CommandSender,
}

impl RpcServerImpl {
    pub fn new(commands: CommandSender) -> Self {
        Self { commands }
    }
}

#[async_trait]
impl RpcApiServer for RpcServerImpl {
    async fn distribute_keys(&self, peer_ids: Vec<String>) -> RpcResult<String> {
        let peer_ids = peer_ids
            .iter()
            .filter_map(|id| PeerId::from_str(id).ok())
            .collect::<Vec<_>>();

        if peer_ids.is_empty() {
            return Err(RpcError::NoPeers.into());
        }

        let (reply, answer) = oneshot::channel();
        let command = RelayerCommand::DistributeKeys {
            peers: peer_ids,
            reply,
        };
        if self.commands.send(command).await.is_err() {
            return Err(RpcError::RelayerStopped.into());
        }

        match answer.await {
            Ok(Ok(())) => Ok("Key distribution started successfully".to_string()),
            Ok(Err(e)) => Err(RpcError::DistributionFailed(e.to_string()).into()),
            Err(_) => Err(RpcError::RelayerStopped.into()),
        }
    }
}

/// Starts the JSON-RPC server on `addr` and returns its handle, which stops
/// the server when dropped, and the address it bound.
pub async fn start_rpc_server(
    commands: CommandSender,
    addr: SocketAddr,
) -> Result<(ServerHandle, SocketAddr), NodeError> {
    let server = ServerBuilder::default()
        .build(addr)
        .await
        .map_err(|e| NodeError::ConfigError(format!("Failed to build RPC server: {}", e)))?;
    let bound = server
        .local_addr()
        .map_err(|e| NodeError::ConfigError(format!("Failed to build RPC server: {}", e)))?;

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

    fn assert_rpc_error(result: RpcResult<String>, message: &str) {
        let error: ErrorObjectOwned = result.unwrap_err();
        assert_eq!(error.code(), 1);
        assert_eq!(error.message(), message);
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

        assert_rpc_error(rpc.distribute_keys(vec![]).await, "No peers provided");
        assert_rpc_error(
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

        assert_rpc_error(
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

        assert_rpc_error(
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

        assert_rpc_error(
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
}
