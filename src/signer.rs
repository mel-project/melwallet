use std::convert::Infallible;

use bytes::Bytes;
use melstructs::Transaction;
use tmelcrypt::Ed25519SK;

/// Represents something that can sign transactions.
pub trait Signer {
    type Error: std::error::Error;

    /// Returns the raw, unhashed covenant that returns true given transactions spent by this signer.
    fn covenant(&self) -> Bytes;

    /// Returns a conservative estimate of the signature size.
    fn sig_size(&self) -> usize;

    /// Signs a transaction. May return an error if the signer refuses to sign the transaction for whatever reason.
    fn sign(&self, txn: &Transaction, for_input: usize) -> Result<Transaction, Self::Error>;
}

/// An ed25519-based signer.
pub struct StdEd25519Signer(pub Ed25519SK);

impl Signer for StdEd25519Signer {
    type Error = Infallible;

    fn covenant(&self) -> Bytes {
        melvm::Covenant::std_ed25519_pk_new(self.0.to_public()).to_bytes()
    }

    fn sig_size(&self) -> usize {
        64
    }

    fn sign(&self, txn: &Transaction, for_input: usize) -> Result<Transaction, Self::Error> {
        let mut txn = txn.clone();
        txn.sigs.resize(for_input.max(txn.sigs.len()), Bytes::new());
        txn.sigs[for_input] = self.0.sign(&txn.hash_nosigs().0).into();
        Ok(txn)
    }
}
