use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use ckb_gen_types::{packed, prelude::*};
use ckb_jsonrpc_types::{Byte32, JsonBytes, Transaction};
use merkle_cbt::CBMT;
use reqwest::{Url, blocking::Client};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use treasury_common::{Hash, MergeHash, transactions_root};

use crate::{ChainBlock, ChainSource};

#[derive(Debug)]
pub enum RpcError {
    InvalidUrl(String),
    Transport(String),
    Protocol { code: i64, message: String },
    MissingResult,
    BlockNotFound(u64),
    InvalidBlock,
}

impl fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(error) => write!(formatter, "invalid CKB RPC URL: {error}"),
            Self::Transport(error) => write!(formatter, "CKB RPC transport error: {error}"),
            Self::Protocol { code, message } => {
                write!(formatter, "CKB RPC error {code}: {message}")
            }
            Self::MissingResult => write!(formatter, "CKB RPC response has no result"),
            Self::BlockNotFound(number) => write!(formatter, "CKB block {number} was not found"),
            Self::InvalidBlock => write!(formatter, "CKB RPC returned an invalid block"),
        }
    }
}

impl std::error::Error for RpcError {}

pub struct CkbRpcClient {
    client: Client,
    endpoint: Url,
    request_id: AtomicU64,
}

impl CkbRpcClient {
    pub fn new(endpoint: &str) -> Result<Self, RpcError> {
        Self::with_proxy(endpoint, true)
    }

    /// Creates a client that bypasses host proxy configuration for a local node.
    pub fn new_direct(endpoint: &str) -> Result<Self, RpcError> {
        Self::with_proxy(endpoint, false)
    }

    fn with_proxy(endpoint: &str, use_proxy: bool) -> Result<Self, RpcError> {
        let endpoint =
            Url::parse(endpoint).map_err(|error| RpcError::InvalidUrl(error.to_string()))?;
        let mut client = Client::builder().timeout(Duration::from_secs(30));
        if !use_proxy {
            client = client.no_proxy();
        }
        let client = client
            .build()
            .map_err(|error| RpcError::Transport(error.to_string()))?;
        Ok(Self {
            client,
            endpoint,
            request_id: AtomicU64::new(1),
        })
    }

    fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Option<T>, RpcError> {
        let id = self.request_id.fetch_add(1, Ordering::Relaxed);
        let response = self
            .client
            .post(self.endpoint.clone())
            .json(&json!({
                "id": id,
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            }))
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| RpcError::Transport(error.to_string()))?
            .json::<JsonRpcResponse<T>>()
            .map_err(|error| RpcError::Transport(error.to_string()))?;
        if let Some(error) = response.error {
            return Err(RpcError::Protocol {
                code: error.code,
                message: error.message,
            });
        }
        Ok(response.result)
    }

    fn fetch_block(&self, block_number: u64) -> Result<ChainBlock, RpcError> {
        let bytes = self
            .call::<JsonBytes>(
                "get_block_by_number",
                json!([format!("0x{block_number:x}"), "0x0", false]),
            )?
            .ok_or(RpcError::BlockNotFound(block_number))?;
        chain_block_from_packed(block_number, &bytes.into_bytes())
    }

    fn send_transaction(&self, transaction: packed::Transaction) -> Result<Hash, RpcError> {
        let transaction = Transaction::from(transaction);
        self.call::<Byte32>("send_transaction", json!([transaction, "passthrough"]))?
            .map(|hash| hash.0)
            .ok_or(RpcError::MissingResult)
    }
}

impl ChainSource for CkbRpcClient {
    type Error = RpcError;

    fn block_by_number(&self, block_number: u64) -> Result<ChainBlock, Self::Error> {
        self.fetch_block(block_number)
    }

    fn submit_transaction(&self, transaction: packed::Transaction) -> Result<Hash, Self::Error> {
        self.send_transaction(transaction)
    }
}

fn chain_block_from_packed(
    expected_number: u64,
    serialized: &[u8],
) -> Result<ChainBlock, RpcError> {
    let (header, transactions) = if let Ok(block) = packed::BlockV1::from_slice(serialized) {
        (
            block.header(),
            block.transactions().into_iter().collect::<Vec<_>>(),
        )
    } else {
        let block = packed::Block::from_slice(serialized).map_err(|_| RpcError::InvalidBlock)?;
        (
            block.header(),
            block.transactions().into_iter().collect::<Vec<_>>(),
        )
    };
    let block_number: u64 = header.raw().number().unpack();
    if block_number != expected_number {
        return Err(RpcError::InvalidBlock);
    }
    if transactions.is_empty() {
        return Err(RpcError::InvalidBlock);
    }
    let raw_hashes = transactions
        .iter()
        .map(|transaction| {
            transaction
                .raw()
                .calc_tx_hash()
                .as_slice()
                .try_into()
                .unwrap()
        })
        .collect::<Vec<Hash>>();
    let witness_hashes = transactions
        .iter()
        .map(|transaction| {
            transaction
                .calc_witness_hash()
                .as_slice()
                .try_into()
                .unwrap()
        })
        .collect::<Vec<Hash>>();
    let raw_root = CBMT::<Hash, MergeHash>::build_merkle_root(&raw_hashes);
    let witnesses_root = CBMT::<Hash, MergeHash>::build_merkle_root(&witness_hashes);
    let header_transactions_root: Hash = header
        .raw()
        .transactions_root()
        .as_slice()
        .try_into()
        .unwrap();
    if transactions_root(raw_root, witnesses_root) != header_transactions_root {
        return Err(RpcError::InvalidBlock);
    }
    Ok(ChainBlock {
        block_number,
        block_hash: header.calc_header_hash().as_slice().try_into().unwrap(),
        parent_hash: header.raw().parent_hash().as_slice().try_into().unwrap(),
        raw_transactions: transactions
            .iter()
            .map(|transaction| transaction.raw().as_slice().to_vec())
            .collect(),
        witnesses_root,
    })
}

#[derive(Deserialize)]
struct JsonRpcResponse<T> {
    result: Option<T>,
    error: Option<JsonRpcFailure>,
}

#[derive(Deserialize)]
struct JsonRpcFailure {
    code: i64,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ckb_gen_types::bytes::Bytes;
    use ckb_gen_types::packed::{Block, RawTransaction, Transaction};

    #[test]
    fn packed_block_adapter_checks_number_and_transactions_root() {
        let transaction = Transaction::new_builder()
            .raw(RawTransaction::new_builder().version(1u32).build())
            .build();
        let transaction_hash: Hash = transaction
            .raw()
            .calc_tx_hash()
            .as_slice()
            .try_into()
            .unwrap();
        let witness_hash: Hash = transaction
            .calc_witness_hash()
            .as_slice()
            .try_into()
            .unwrap();
        let root = transactions_root(transaction_hash, witness_hash);
        let block = Block::new_builder()
            .header(
                packed::Header::new_builder()
                    .raw(
                        packed::RawHeader::new_builder()
                            .number(42u64)
                            .transactions_root(root.pack())
                            .build(),
                    )
                    .build(),
            )
            .transactions([transaction].pack())
            .build();

        let adapted = chain_block_from_packed(42, block.as_slice()).unwrap();
        assert_eq!(adapted.block_number, 42);
        assert_eq!(adapted.witnesses_root, witness_hash);
        assert_eq!(adapted.raw_transactions.len(), 1);
        assert!(chain_block_from_packed(41, block.as_slice()).is_err());

        let block_v1 = packed::BlockV1::new_builder()
            .header(block.header())
            .transactions(block.transactions())
            .extension(Bytes::new().pack())
            .build();
        let adapted_v1 = chain_block_from_packed(42, block_v1.as_slice()).unwrap();
        assert_eq!(adapted_v1, adapted);
    }
}
