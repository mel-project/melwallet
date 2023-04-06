mod signer;
use bytes::Bytes;
use serde_with::serde_as;
pub use signer::*;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
};

use melstructs::{
    Address, BlockHeight, CoinData, CoinDataHeight, CoinID, Denom, Transaction, TxHash, TxKind,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A [Wallet] is a bookkeeping struct to keep track of all the coins locked by a particular covenant.
#[derive(Clone, Debug)]
pub struct Wallet {
    /// The address (covenant hash) that all the coins here are associated with.
    pub address: Address,
    /// The latest block height known to this wallet.
    pub height: BlockHeight,
    /// All the *confirmed* UTXOs: output coins of confirmed transactions that this wallet can spend.
    pub confirmed_utxos: BTreeMap<CoinID, CoinDataHeight>,
    /// Pending outgoing transactions. These transactions' outputs may be further spent in more transactions, but they aren't confirmed yet. We use a map in order to ensure deduplication.
    pub pending_outgoing: BTreeMap<TxHash, Transaction>,
}

#[derive(Error, Debug)]
pub enum AddCoinsError {
    #[error("height is not contiguous to the existing height")]
    BadHeight,

    #[error("address of added coins does not match the wallet address")]
    WrongAddress,
}

impl Wallet {
    /// Adds all the coin diffs at a particular block height.
    pub fn add_coins(
        &mut self,
        height: BlockHeight,
        new_coins: impl IntoIterator<Item = (CoinID, CoinData)>,
        spent_coins: impl IntoIterator<Item = CoinID>,
    ) -> Result<(), AddCoinsError> {
        if height != self.height + BlockHeight(1) {
            return Err(AddCoinsError::BadHeight);
        }
        let spent_coins = spent_coins.into_iter().collect::<HashSet<_>>();

        // we put everything in a temporary hashmap, so that if things fail we don't leave the wallet in a bad state
        let mut accum = HashMap::new();
        for (coin_id, coin_data) in new_coins.into_iter() {
            if coin_data.covhash != self.address {
                return Err(AddCoinsError::WrongAddress);
            }
            accum.insert(coin_id, CoinDataHeight { coin_data, height });
        }

        // update the wallet itself
        for (k, v) in accum {
            // the originating transaction of this coin must no longer be pending
            self.pending_outgoing.remove(&k.txhash);
            self.confirmed_utxos.insert(k, v);
        }
        for k in spent_coins {
            self.confirmed_utxos.remove(&k);
        }
        Ok(())
    }

    /// Prepare a transaction. Attempts to produce a signed transaction,
    pub fn prepare_tx<S: Signer>(
        &self,
        args: PrepareTxArgs,
        signer: &S,
    ) -> Result<Transaction, PrepareTxError<S::Error>> {
        todo!()
    }
}

#[derive(Error, Debug, Serialize, Deserialize)]
/// The error type returned by [crate::MelwalletdProtocol::prepare_tx].
pub enum PrepareTxError<E: Error> {
    #[error("not enough money (more of {0} needed)")]
    InsufficientFunds(Denom),

    #[error("cannot spend external input coin {0}")]
    BadExternalInput(CoinID),

    #[error("signer refused to sign with error: {0}")]
    SignerRefused(E),
}

#[serde_as]
#[derive(Debug, Serialize, Deserialize, Clone)]
/// Arguments passed to [crate::MelwalletdProtocol::prepare_tx]. Configures what sort of transaction to construct.
pub struct PrepareTxArgs {
    /// "Kind" of the transaction.
    pub kind: TxKind,
    /// **Additional** inputs of the transaction. Normally, this field can be left as an empty vector, in which case UTXOs locked by the wallet's own address are picked automatically.
    ///
    /// Use this field to specify "out of wallet" coins from dapps, multisig vaults, and such, which do not have their `covhash` field equal to the [Address] of the wallet, yet the wallet is able to spend, possibly in combination with other fields of [PrepareTxArgs]. For example, a multisig coin would not have the [Address] of any single-key wallet, and spending it must require explicitly specifying its [CoinID] and explicitly passing unlock arguments.
    ///
    /// Optional in JSON, in which case it defaults to an empty list.
    #[serde(default)]
    pub inputs: Vec<(CoinID, CoinDataHeight)>,
    /// **Required** outputs of the transaction. This generally specifies the "recipients" of the transaction. Note that this only specifies the first outputs of the transaction; more outputs may be created as "change" outputs.
    pub outputs: Vec<CoinData>,
    /// **Additional** covenants that must be included in the transaction. This is needed when spending out-of-wallet coins. Optional in JSON, defaulting to an empty list.
    #[serde(default)]
    #[serde_as(as = "Vec<stdcode::HexBytes>")]
    pub covenants: Vec<Bytes>,
    /// The "data" field of the transaction. Optional and hex-encoded in JSON, defaulting to an empty string.
    #[serde(default)]
    #[serde_as(as = "stdcode::HexBytes")]
    pub data: Bytes,

    #[serde(default)]
    /// Pretend like the transaction has this many more bytes when calculating the correct fee level. Useful in niche situations where you want to intentionally pay more fees than necessary.
    pub fee_ballast: usize,
}

impl Default for PrepareTxArgs {
    fn default() -> Self {
        Self {
            kind: TxKind::Normal,
            inputs: vec![],
            outputs: vec![],
            covenants: vec![],
            data: Default::default(),
            fee_ballast: 0,
        }
    }
}
