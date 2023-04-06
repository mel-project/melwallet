use bytes::Bytes;
use melstructs::Transaction;

/// Represents something that can sign transactions.
pub trait Signer {
    type Error: std::error::Error;

    /// Returns the raw, unhashed covenant that returns true given transactions spent by this signer.
    fn covenant(&self) -> Bytes;

    /// Signs a transaction. May return an error if the signer refuses to sign the transaction for whatever reason.
    fn sign(&self, txn: &Transaction) -> Result<Transaction, Self::Error>;
}
