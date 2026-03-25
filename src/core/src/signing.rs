use crate::wallet::Wallet;
use crate::transaction::Transaction;
use borsh::BorshSerialize;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

fn tx_signable_bytes(tx: &Transaction) -> Vec<u8> {
    let mut bytes = Vec::new();
    tx.from.serialize(&mut bytes).unwrap();
    tx.nonce.serialize(&mut bytes).unwrap();
    tx.kind.serialize(&mut bytes).unwrap();
    bytes
}

pub fn sign_transaction(tx: &mut Transaction, wallet: &Wallet) {
    let bytes = tx_signable_bytes(tx);
    let sig = wallet.sign(&bytes);
    tx.signature = sig.to_bytes().to_vec();
}

pub fn verify_transaction(tx: &Transaction, public_key: &VerifyingKey) -> bool {
    if tx.signature.len() != 64 {
        return false;
    }
    let bytes = tx_signable_bytes(tx);
    let sig_bytes: &[u8; 64] = match tx.signature.as_slice().try_into() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let sig = Signature::from_bytes(sig_bytes);
    public_key.verify(&bytes, &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::Wallet;
    use crate::transaction::{Transaction, TxKind};
    use crate::token::Amount;
    use crate::identity::Address;

    #[test]
    fn sign_and_verify_transfer() {
        let sender = Wallet::generate();
        let recipient = Address([1u8; 32]);

        let mut tx = Transaction {
            from: *sender.address(),
            nonce: 0,
            kind: TxKind::Transfer {
                to: recipient,
                amount: Amount::from_comme(10),
            },
            signature: vec![],
        };

        sign_transaction(&mut tx, &sender);
        assert!(!tx.signature.is_empty());
        assert!(verify_transaction(&tx, sender.public_key()));
    }

    #[test]
    fn tampered_tx_fails_verification() {
        let sender = Wallet::generate();
        let recipient = Address([1u8; 32]);

        let mut tx = Transaction {
            from: *sender.address(),
            nonce: 0,
            kind: TxKind::Transfer {
                to: recipient,
                amount: Amount::from_comme(10),
            },
            signature: vec![],
        };

        sign_transaction(&mut tx, &sender);
        tx.nonce = 999;
        assert!(!verify_transaction(&tx, sender.public_key()));
    }
}
