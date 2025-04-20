#![allow(unused)]
#![allow(unused_imports)]
#![allow(unused_import_braces)]
#![allow(clippy::print_stdout)]

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use bitcoin::{Address, Amount, Network, OutPoint, Psbt, Transaction, TxIn, TxOut, Txid};

use bdk_chain::CanonicalizationParams;
use bdk_wallet::{chain as bdk_chain, rusqlite, test_utils::*, KeychainKind::*, Wallet};

const RECEIV: &str = "tr(tprv8ZgxMBicQKsPdy6LMhUtFHAgpocR8GC6QmwMSFpZs7h6Eziw3SpThFfczTDh5rW2krkqffa11UpX3XkeTTB2FvzZKWXqPY54Y6Rq4AQ5R8L/86h/1h/0h/0/*)";
const CHANGE: &str = "tr(tprv8ZgxMBicQKsPdy6LMhUtFHAgpocR8GC6QmwMSFpZs7h6Eziw3SpThFfczTDh5rW2krkqffa11UpX3XkeTTB2FvzZKWXqPY54Y6Rq4AQ5R8L/86h/1h/0h/1/*)";
const NETWORK: Network = Network::Regtest;

// Create a "peel-chain" by building a series of dependent txs and broadcasting each one in turn.

fn main() -> anyhow::Result<()> {
    let mut conn = rusqlite::Connection::open_in_memory()?;

    let mut wallet = match Wallet::load()
        .descriptor(External, Some(RECEIV))
        .descriptor(Internal, Some(CHANGE))
        .extract_keys()
        .load_wallet(&mut conn)?
    {
        Some(w) => w,
        None => Wallet::create(RECEIV, CHANGE)
            .network(NETWORK)
            .create_wallet(&mut conn)?,
    };

    // Receive a single large UTXO
    let outpoint_0 = fund_wallet(&mut wallet, Amount::ONE_BTC);
    assert_eq!(wallet.balance().total(), Amount::ONE_BTC);
    println!("Funding txid {}", outpoint_0.txid);

    // Setup common tx params
    let to_send = Amount::from_sat(1_000_000);
    let fee = Amount::from_sat(256);
    let recipients: Vec<Address> = [
        "bcrt1qq6mn7gvd80kl4ld0a9uqctcw7nv4ya9gln5hf2",
        "bcrt1qtc8dqzp2zuys80pqzhm95yhw9qhxsyk86hrcvt",
        "bcrt1qg8ex4wjpzx6kec8cfv46ha7m2afhf7ymtchfmj",
        "bcrt1q4quq7rauyzvzh5x6clqgam3drsr5jsxql05vw9",
        "bcrt1qsprm8wl52qh2q8p64t0njg6pgg8kgh9eye3t22",
        // "bcrt1qtycvqahl7apcerrkym28qrkrr3jk3jq98lha9p",
        // "bcrt1q7ffyy4mgljv4hmzx6uuc25smz5sgnfgrwttctw",
        // "bcrt1qmsy09k4a0yv8x6r2cag9l694cdy8stexpwe7qy",
        // "bcrt1q9lkhxupgtrejk3uaf4q6u4gv442d26ce99lulq",
        // "bcrt1qz363v6n8sggx3x2g65xqmhaky984xxxpgrm6zt",
        // "bcrt1q96tuj8t5hwkm0rd9l9gtn4qhsfrqfrmq50st35"
    ]
    .into_iter()
    .map(|s| {
        Address::from_str(s)
            .expect("must be valid Address")
            .assume_checked()
    })
    .collect();

    // All created PSBTs go here
    let mut psbts = HashMap::<Txid, Psbt>::new();

    // Create tx 1 (recipient 0)
    let mut builder = wallet.build_tx();
    builder
        .include_input(outpoint_0)
        .manually_selected_only()
        .add_recipient(recipients[0].script_pubkey(), to_send)
        .fee_absolute(fee);

    let psbt = builder.finish()?;
    let txid = psbt.unsigned_tx.compute_txid();
    psbts.insert(txid, psbt.clone());
    println!("Txid 0 {}", txid);

    let mut last_tx = psbt.unsigned_tx;
    let mut last_txid = txid;

    for (i, _) in recipients.iter().enumerate().skip(1) {
        // Locate the change output
        let outpoint = last_tx
            .output
            .iter()
            .enumerate()
            .find(|(_, txo)| {
                matches!(
                    wallet.spk_index().index_of_spk(txo.script_pubkey.clone()),
                    Some(&(keychain, _))
                    if keychain == Internal,
                )
            })
            .map(|(vout, _)| OutPoint::new(last_txid, vout as u32))
            .expect("should find change output");

        // Build the next tx assuming the last one is canonical
        let mut builder = wallet.build_tx();
        builder
            .include_input(outpoint)
            .manually_selected_only()
            .add_recipient(recipients[i].script_pubkey(), to_send)
            .fee_absolute(fee);
        let psbt = builder.finish()?;
        let txid = psbt.unsigned_tx.compute_txid();
        println!("Txid {i} {txid}");
        // update for next iteration
        last_tx = psbt.unsigned_tx.clone();
        last_txid = txid;
        psbts.insert(txid, psbt);
    }

    // Persist and reload
    wallet.persist(&mut conn)?;

    wallet = Wallet::load()
        .descriptor(External, Some(RECEIV))
        .descriptor(Internal, Some(CHANGE))
        .extract_keys()
        .load_wallet(&mut conn)?
        .expect("wallet was persisted");

    assert_eq!(wallet.tx_graph().full_txs().count(), recipients.len() + 1);

    // Now retrieve the list of unbroadcast(ed) txs. We assume that the last
    // descendant is canonical, which will pick up the ancestors as well. When
    // we go to broadcast, we will pop items from this stack.
    let mut txs_to_send: Vec<Txid> = wallet
        .transactions_with_params(CanonicalizationParams {
            assume_canonical: vec![last_txid],
        })
        // keep only unsigned
        .filter_map(|canon_tx| {
            if is_unsigned(&canon_tx.tx_node.tx) {
                Some(canon_tx.tx_node.txid)
            } else {
                None
            }
        })
        .take(recipients.len())
        .collect();

    assert_eq!(txs_to_send.len(), recipients.len());

    // For each tx to send
    while let Some(txid) = txs_to_send.pop() {
        // Get the corresponding psbt
        let mut psbt = psbts.get(&txid).cloned().expect("must have psbt");

        // Sign it
        assert!(wallet.sign(&mut psbt, bdk_wallet::SignOptions::default())?);

        // Re-insert the final tx into the wallet
        let tx = Arc::new(psbt.extract_tx()?);
        let created_at = std::time::UNIX_EPOCH.elapsed()?.as_secs();
        wallet.apply_unconfirmed_txs([(Arc::clone(&tx), created_at)]);

        // Prepare to broadcast
        println!("{}", bitcoin::consensus::encode::serialize_hex(&tx));
    }

    Ok(())
}

/// Whether the wallet `tx` is still unsigned.
fn is_unsigned(tx: &Arc<Transaction>) -> bool {
    tx.input
        .iter()
        .all(|txin| txin.script_sig.is_empty() && txin.witness.is_empty())
}

/// Add `value` amount of BTC to the wallet.
fn fund_wallet(wallet: &mut Wallet, value: Amount) -> OutPoint {
    let tx = Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![TxIn::default()],
        output: vec![TxOut {
            value,
            script_pubkey: wallet.reveal_next_address(External).script_pubkey(),
        }],
    };
    let txid = tx.compute_txid();
    let outpoint = OutPoint::new(txid, 0);
    insert_tx(wallet, tx);
    outpoint
}
