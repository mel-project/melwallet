mod signer;
use bytes::Bytes;
use serde_with::{serde_as, Same};
pub use signer::*;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
};

use melstructs::{
    Address, BlockHeight, CoinData, CoinDataHeight, CoinID, CoinValue, Denom, NetID, Transaction,
    TxHash, TxKind,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A [Wallet] is a bookkeeping struct to keep track of all the coins locked by a particular covenant.
#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Wallet {
    /// NetID of this wallet
    pub netid: NetID,
    /// The address (covenant hash) that all the coins here are associated with.
    pub address: Address,
    /// The latest block height known to this wallet.
    pub height: BlockHeight,
    #[serde_as(as = "Vec<(Same, Same)>")]
    /// All the *confirmed* UTXOs: output coins of confirmed transactions that this wallet can spend.
    pub confirmed_utxos: BTreeMap<CoinID, CoinDataHeight>,
    #[serde_as(as = "Vec<(Same, Same)>")]
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
    /// Lists the balances of the wallet, by token.
    pub fn balances(&self) -> BTreeMap<Denom, CoinValue> {
        self.confirmed_utxos
            .values()
            .fold(BTreeMap::new(), |mut map, cdh| {
                map.entry(cdh.coin_data.denom).or_default().0 += cdh.coin_data.value.0;
                map
            })
    }

    /// Adds all the coin diffs at a particular block height. Clears pending transactions that the coin diffs show are
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
        self.height = height;
        Ok(())
    }

    /// Reset the wallet to a certain set of coins.
    pub fn full_reset(
        &mut self,
        latest_height: BlockHeight,
        confirmed_utxos: impl IntoIterator<Item = (CoinID, CoinDataHeight)>,
    ) -> Result<(), AddCoinsError> {
        let confirmed_utxos: BTreeMap<CoinID, CoinDataHeight> =
            confirmed_utxos.into_iter().collect();

        // Verify that the inputs have the correct address
        for (_, coin_data_height) in confirmed_utxos.iter() {
            if coin_data_height.coin_data.covhash != self.address {
                return Err(AddCoinsError::WrongAddress);
            }
        }

        self.height = latest_height;
        self.confirmed_utxos = confirmed_utxos;
        self.pending_outgoing.clear();
        Ok(())
    }

    /// Prepare a transaction. Attempts to produce a signed transaction that fits the constraints given by the arguments.
    pub fn prepare_tx<S: Signer>(
        &self,
        args: PrepareTxArgs,
        signer: &S,
        fee_multiplier: u128,
        check_balanced: bool,
    ) -> Result<Transaction, PrepareTxError<S::Error>> {
        // Exponentially increase the fees until we either run out of money, or we have enough fees.
        for power in 0.. {
            let fee = CoinValue(1.1f64.powi(power) as _);
            // Tally up the total outputs
            let mut inmoney_needed: BTreeMap<Denom, CoinValue> =
                args.outputs
                    .iter()
                    .fold(BTreeMap::new(), |mut map, output| {
                        if output.denom != Denom::NewCustom {
                            *map.entry(output.denom).or_default() += output.value;
                        }
                        map
                    });
            *inmoney_needed.entry(Denom::Mel).or_default() += fee;
            // pick out input UTXOs until we have enough, then construct a Transaction
            let mut to_spend = args.inputs.clone();
            let mut inmoney_actual: BTreeMap<Denom, CoinValue> =
                to_spend.iter().fold(BTreeMap::new(), |mut map, (_, cdh)| {
                    *map.entry(cdh.coin_data.denom).or_default() += cdh.coin_data.value;

                    map
                });
            let mut touched_coin_count = 0;
            for (denom, needed) in inmoney_needed.iter() {
                for (in_coinid, in_cdh) in self
                    .spendable_utxos()
                    .filter(|(_, v)| &v.coin_data.denom == denom)
                {
                    if inmoney_actual.get(denom).copied().unwrap_or_default() < *needed {
                        touched_coin_count += 1;
                        to_spend.push((*in_coinid, in_cdh.clone()));
                        *inmoney_actual.entry(*denom).or_default() += in_cdh.coin_data.value;
                    } else {
                        break;
                    }
                }
            }
            // produce change outputs
            let mut outputs = args.outputs.clone();
            if *inmoney_actual.entry(Denom::Mel).or_default() >= fee {
                return Err(PrepareTxError::InsufficientFunds(Denom::Mel)); // you always need MEL to pay the transaction fee
            }

            for (denom, inmoney) in &inmoney_actual {
                if let Some(change_value) =
                    inmoney.checked_sub(inmoney_needed.get(denom).copied().unwrap_or(CoinValue(0)))
                {
                    if change_value > CoinValue(0) {
                        outputs.push(CoinData {
                            covhash: self.address,
                            denom: *denom,
                            value: change_value,
                            additional_data: Bytes::new(),
                        });
                    }
                } else {
                    if check_balanced {
                        return Err(PrepareTxError::InsufficientFunds(*denom));
                    }
                }
            }

            // assemble the transaction
            let mut assembled = Transaction {
                kind: args.kind,
                inputs: to_spend.iter().map(|s| s.0).collect(),
                outputs,
                fee,
                covenants: std::iter::repeat(signer.covenant())
                    .take(to_spend.len())
                    .collect(),
                data: args.data.clone(),
                sigs: std::iter::repeat(Bytes::from(vec![0; signer.sig_size()]))
                    .take(to_spend.len())
                    .collect(),
            };
            if assembled
                .base_fee(
                    fee_multiplier,
                    args.fee_ballast as u128,
                    melvm::covenant_weight_from_bytes,
                )
                .0
                <= fee.0
            {
                assembled.sigs.clear();
                let signed = (0..(args.inputs.len() + touched_coin_count))
                    .try_fold(assembled, |tx, i| signer.sign(&tx, i))?;
                return Ok(signed);
            }
        }
        Err(PrepareTxError::InsufficientFunds(Denom::Mel))
    }

    /// Note a pending, outgoing transaction. This should be called *after* this transaction has been sent successfully to the network, and the main effect is to prevent the wallet from using the coins that the transaction spent, even before that transaction confirms.
    pub fn add_pending(&mut self, tx: Transaction) {
        self.pending_outgoing.insert(tx.hash_nosigs(), tx);
    }

    fn spendable_utxos(&self) -> impl Iterator<Item = (&CoinID, &CoinDataHeight)> + '_ {
        self.confirmed_utxos.iter().filter(|(k, _)| {
            // filter out the coins that a pending output is trying to spend
            !self
                .pending_outgoing
                .iter()
                .any(|(_, tx)| tx.inputs.iter().any(|pending_input| &pending_input == k))
        })
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
    SignerRefused(#[from] E),
}

#[serde_as]
#[derive(Debug, Serialize, Deserialize, Clone)]
/// Constraints on what sort of transaction to prepare.
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
