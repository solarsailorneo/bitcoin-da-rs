use bitcoin::amount::Amount;
use bitcoin::blockdata::script::Builder;
use bitcoin::hash_types::Txid;
use bitcoin::BlockHash;

use bitcoin::address::AddressType;
use bitcoin::key::PrivateKey;
use bitcoin::opcodes;
use bitcoin::script as txscript;
use bitcoin::script::Instruction;
use bitcoin::script::PushBytesBuf;
use bitcoin::secp256k1::KeyPair;
use bitcoin::secp256k1::XOnlyPublicKey;
use bitcoin::secp256k1::{All, Secp256k1};
use bitcoin::taproot::LeafVersion;
use bitcoin::taproot::NodeInfo;
use bitcoin::taproot::TapTree;
use bitcoin::taproot::TaprootBuilder;

use bitcoin::OutPoint;
use bitcoin::ScriptBuf;
use bitcoin::Transaction;
use bitcoin::Witness;
use bitcoin::{Address, Network};
use bitcoin::{TxIn, TxOut};
use bitcoincore_rpc::Auth;
use bitcoincore_rpc::Client as RpcClient;
use bitcoincore_rpc::Error;
use bitcoincore_rpc::RpcApi;
// Standard imports
use core::fmt;

pub const PROTOCOL_ID: [u8; 4] = [0x62, 0x61, 0x72, 0x6b]; // 'bark' in ASCII

// Internal key pair is used for tweaking
pub const INTERNAL_PRIVATE_KEY: &str = "5JGgKfRy6vEcWBpLJV5FXUfMGNXzvdWzQHUM1rVLEUJfvZUSwvS";

#[derive(Debug)]
pub enum BitcoinError {
    InvalidAddress,
    SendToAddressError,
    BadAmount,
    PrivateKeyErr,
    InvalidTxHash,
    ControlBlockErr,
    TransactionErr,
    RevealErr,
    InvalidNetwork,
    ReadErr,
    ReadNoDataErr,
    GetBlockErr,
    GetBlockchainInfoErr,
}

impl fmt::Display for BitcoinError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            BitcoinError::InvalidAddress => write!(f, "Invalid address"),
            BitcoinError::SendToAddressError => write!(f, "Send to address error"),
            BitcoinError::BadAmount => write!(f, "Amount parsing error"),
            BitcoinError::PrivateKeyErr => write!(f, "Private key error"),
            BitcoinError::InvalidTxHash => write!(f, "Invalid transaction hash"),
            BitcoinError::ControlBlockErr => write!(f, "Control block error"),
            BitcoinError::TransactionErr => write!(f, "Transaction error"),
            BitcoinError::RevealErr => write!(f, "Reveal error"),
            BitcoinError::InvalidNetwork => write!(f, "Invalid network"),
            BitcoinError::ReadErr => write!(f, "Read error"),
            BitcoinError::ReadNoDataErr => write!(f, "Read no data in tx error"),
            BitcoinError::GetBlockErr => write!(f, "Get block error"),
            BitcoinError::GetBlockchainInfoErr => write!(f, "Get blockchain info error"),
        }
    }
}

/// Splits a slice of data into multiple chunks of a given size.
/// # Arguments
/// * `slice` - The slice to be divided.
/// * `chunk_size` - The maximum size of each chunk.
/// # Returns
/// A vector containing slices, each representing a chunk of the original slice.
pub fn chunk_slice(slice: &[u8], chunk_size: usize) -> Vec<&[u8]> {
    let mut chunks = Vec::new();

    // Use a range to iterate over the slice in steps of `chunk_size`
    for i in (0..slice.len()).step_by(chunk_size) {
        // Calculate the end of the current chunk
        let end = usize::min(i + chunk_size, slice.len());

        // Push the chunk into the vector
        chunks.push(&slice[i..end]);
    }

    chunks
}

/// Builds a Bitcoin transaction script with specific embedded data.
/// The function creates a transaction script that starts with an `OP_FALSE`
/// followed by an `OP_IF` structure containing some predefined data, and then
/// embeds the provided data in chunks (of a max size of 520 bytes each).
/// # Arguments
/// * `embedded_data` - The data to be embedded into the transaction script.
/// # Returns
/// A `txscript::Builder` containing the formed Bitcoin script.
pub fn build_script(embedded_data: &[u8]) -> txscript::Builder {
    let mut builder = txscript::Builder::new();
    builder = builder
        // push false onto the stack
        .push_opcode(opcodes::OP_FALSE)
        // if false
        .push_opcode(opcodes::all::OP_IF)
        // push "block" onto the stack
        .push_slice(PushBytesBuf::try_from("block".as_bytes().to_vec()).unwrap())
        // push 1 onto the stack
        .push_int(1)
        // replace by actual block height
        .push_slice(PushBytesBuf::try_from("block_height".as_bytes().to_vec()).unwrap())
        // push 0 onto the stack
        .push_opcode(opcodes::OP_0);
    let chunks = chunk_slice(embedded_data, 520); // chunk the data into 520 byte chunks
    for chunk in chunks {
        // for each chunk
        // push the chunk onto the stack
        builder = builder.push_slice(PushBytesBuf::try_from(chunk.to_vec()).unwrap());
    }
    // end if
    builder = builder.push_opcode(opcodes::all::OP_ENDIF);
    // push true onto the stack
    let builder: txscript::Builder = builder.push_opcode(opcodes::OP_TRUE);

    builder
}

/// Creates a Bitcoin Taproot address with specific embedded data.
/// The function generates a taproot address using a predefined internal private key
/// and a given script derived from the embedded data.
/// # Arguments
/// * `embedded_data` - The data to be embedded into the taproot script.
/// * `network` - The Bitcoin network (mainnet, testnet, regtest, etc.)
/// # Returns
/// A `Result` which is either the generated `Address` or a `BitcoinError`.
pub fn create_taproot_address(
    embedded_data: &[u8],
    network: Network,
) -> Result<Address, BitcoinError> {
    // Initialize the secp256k1 context
    let secp = &Secp256k1::<All>::new();

    // Retrieve the internal private key and derive the key pair
    let internal_pkey = PrivateKey::from_wif(INTERNAL_PRIVATE_KEY).unwrap();
    let key_pair = KeyPair::from_secret_key(secp, &internal_pkey.inner);
    let (x_pub_key, _) = XOnlyPublicKey::from_keypair(&key_pair);

    // Construct the taproot script using the provided embedded data
    let builder: txscript::Builder = build_script(embedded_data);
    let pk_script = builder.as_script();

    // Create the taproot tree and derive the output key for the address
    let mut taproot_builder = TaprootBuilder::new();
    taproot_builder = taproot_builder.add_leaf(0, pk_script.into()).unwrap();
    let tap_tree = taproot_builder.finalize(secp, x_pub_key).unwrap();
    let output_key = tap_tree.output_key();

    // Generate and return the taproot address
    Ok(Address::p2tr_tweaked(output_key, network))
}

pub fn pay_to_taproot_script(taproot_key: &XOnlyPublicKey) -> Result<ScriptBuf, String> {
    let builder = Builder::new()
        .push_opcode(opcodes::all::OP_PUSHNUM_1)
        .push_slice(taproot_key.serialize())
        .into_script();
    Ok(builder)
}

/// Finds the index and output of the commit transaction from the given `txid`.
///
/// The function retrieves the raw transaction for the given `txid` and
/// searches for an output with a value of 100,000 (presumed to be the fee amount).
/// It returns the index and `TxOut` of the found commit transaction output.
///
/// # Arguments
///
/// * `txid` - The transaction ID to search for.
/// * `client` - The RPC client to fetch the raw transaction.
///
/// # Returns
///
/// A `Result` which is either a tuple of the index and `TxOut` of the
/// commit transaction output, or a `BitcoinError`.
pub fn find_commit_idx_output_from_txid(
    txid: &Txid,
    client: &RpcClient,
) -> Result<(usize, TxOut), BitcoinError> {
    // Attempt to fetch the raw transaction using the provided `txid`
    let raw_commit = match client.get_raw_transaction(txid, None) {
        Ok(tx) => Ok(tx),
        Err(_) => match client.get_transaction(txid, None) {
            Ok(get_tx_result) => get_tx_result
                .transaction()
                .map_err(|_| BitcoinError::InvalidTxHash),
            Err(err) => {
                eprintln!("Error: {:?}", err);
                Err(BitcoinError::InvalidTxHash)
            }
        },
    }?;

    // Search for the desired UTXO in the transaction outputs
    for (i, out) in raw_commit.output.iter().enumerate() {
        // Identify the output with the value 10,000 (assuming this is the fee amount)
        if out.value == 10000 {
            return Ok((i, out.clone()));
        }
    }

    // If no matching output is found, return an error
    Err(BitcoinError::TransactionErr)
}

// Relayer is a bitcoin client wrapper which provides reader and writer methods
// to write binary blobs to the blockchain.
pub struct Relayer {
    pub client: RpcClient,
}

impl Relayer {
    // NewRelayer creates a new Relayer instance with the provided Config.
    pub fn new(config: &Config) -> Result<Self, Error> {
        // Set up the connection to the bitcoin RPC server.
        // NOTE: for testing bitcoind can be used in regtest with the following params -
        // bitcoind -chain=regtest -rpcport=8332 -rpcuser=rpcuser -rpcpassword=rpcpass -fallbackfee=0.000001 -txindex=1
        let auth = Auth::UserPass(config.user.clone(), config.pass.clone());
        let client = RpcClient::new(&config.host, auth)?;

        Ok(Relayer { client })
    }

    // close shuts down the client.
    pub fn close(&self) {
        let shutdown = self.client.stop();
        match shutdown {
            Ok(stop_message) => {
                println!("Shutdown client : {}", stop_message);
            }
            Err(error) => {
                println!("Failed to stop client : {}", error);
            }
        }
    }

    /// Sends a fixed amount to a P2TR address and returns the transaction ID.
    /// commitTx commits an output to the given taproot address, such that the
    /// output is only spendable by posting the embedded data on chain, as part of
    /// the script satisfying the tapscript spend path that commits to the data.
    ///
    /// # Arguments
    ///
    /// * `addr` - The address to send to.
    ///
    /// # Returns
    ///
    /// A `Result` which is either the transaction ID of the commit, or a `BitcoinError`.
    pub fn commit_tx(&self, addr: &Address) -> Result<Txid, BitcoinError> {
        match addr.address_type() {
            Some(AddressType::P2tr) => {
                // fee to cover the cost
                let amount = Amount::from_btc(0.0001).map_err(|_| BitcoinError::BadAmount)?;
                let hash: Txid = self
                    .client
                    .send_to_address(
                        addr,
                        amount,
                        None,
                        None,
                        Some(false),
                        Some(true),
                        None,
                        None,
                    )
                    .map_err(|err| {
                        eprintln!("Error: {:?}", err);
                        BitcoinError::SendToAddressError
                    })?;
                Ok(hash)
            }
            _ => Err(BitcoinError::InvalidAddress),
        }
    }

    /// Creates and sends a reveal transaction.
    /// Spends the output from the commit transaction and as part of the
    /// script satisfying the tapscript spend path, posts the embedded data on chain.
    /// # Arguments
    ///
    /// * `embedded_data` - Data to embed in the transaction.
    /// * `commit_hash` - Commitment transaction ID.
    ///
    /// # Returns
    ///
    /// A `Result` which is either the transaction ID of the reveal or a `BitcoinError`.
    pub fn reveal_tx(
        &self,
        embedded_data: &[u8],
        commit_hash: &Txid,
    ) -> Result<Txid, BitcoinError> {
        // Retrieve the index and output of the commit transaction
        let (commit_idx, commit_output) =
            find_commit_idx_output_from_txid(commit_hash, &self.client).unwrap();

        // Initialize the secp256k1 context
        let secp = &Secp256k1::<All>::new();

        // Derive the public key from a known private key
        let internal_prkey = PrivateKey::from_wif(INTERNAL_PRIVATE_KEY).unwrap();
        let internal_pub_key = internal_prkey.public_key(secp);
        let x_pub_key: XOnlyPublicKey = XOnlyPublicKey::from(internal_pub_key.inner);

        // Create the taproot script using the embedded data
        let builder: txscript::Builder = build_script(embedded_data);
        let pk_script = builder.as_script();

        // Construct the Taproot tree
        let mut taproot_builder = TaprootBuilder::new();
        taproot_builder = taproot_builder.add_leaf(0, pk_script.into()).unwrap();
        let tap_tree = taproot_builder.finalize(secp, x_pub_key).unwrap();
        let output_key = tap_tree.output_key();

        // Prepare the reveal transaction
        let mut tx = Transaction {
            version: 2,
            lock_time: bitcoin::locktime::absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: *commit_hash,
                    vout: commit_idx as u32,
                },
                script_sig: ScriptBuf::new(),
                sequence: bitcoin::Sequence::MAX,
                witness: Witness::new(),
            }],
            output: Vec::new(),
        };

        // Ensure the output key matches the commit output and the P2TR script
        let p2tr_script = pay_to_taproot_script(&output_key.to_inner()).unwrap();
        assert_eq!(p2tr_script, commit_output.script_pubkey);

        // Define the transaction's output
        let tx_out = TxOut {
            value: 1000, // in satoshi
            script_pubkey: p2tr_script,
        };
        tx.output.push(tx_out);

        // Generate the control block for the witness
        let control_block = tap_tree
            .control_block(&((pk_script.into()), LeafVersion::TapScript))
            .ok_or(BitcoinError::ControlBlockErr)
            .unwrap();

        // Assemble the transaction witness
        tx.input[0].witness.push(pk_script.as_bytes());
        tx.input[0].witness.push(control_block.serialize());

        // Send the raw transaction and handle any errors
        let txid = self.client.send_raw_transaction(&tx);
        match txid {
            Ok(hash) => Ok(hash),
            Err(err) => {
                eprintln!("Error: {:?}", err);
                Err(BitcoinError::RevealErr)
            }
        }
    }

    /// Reads and extracts data from a transaction's witness.
    ///
    /// # Arguments
    ///
    /// * `hash` - The transaction ID.
    /// * `block_hash` - Optional block hash where the transaction is located.
    ///
    /// # Returns
    ///
    /// A `Result` containing the extracted data as a vector of bytes,
    /// or a `BitcoinError` if something went wrong.    
    pub fn read_transaction(
        &self,
        hash: &Txid,
        block_hash: Option<&BlockHash>,
    ) -> Result<Vec<u8>, BitcoinError> {
        // Get the raw transaction
        let tx = self
            .client
            .get_raw_transaction(hash, block_hash)
            .map_err(|_| BitcoinError::InvalidTxHash)?;

        let mut data = Vec::new();

        // Iterate over transaction inputs
        for input in tx.input.iter() {
            // Try to get the second to last witness data
            if let Some(wit_data) = input.witness.second_to_last() {
                // Extract data from the witness
                if let Some(mut extracted_data) = extract_push_data(wit_data.to_vec()) {
                    data.append(&mut extracted_data);
                }
            }
        }

        Ok(data)
    }

    /// Reads and extracts data from transaction witnesses within a block at a given height.
    ///
    /// # Arguments
    ///
    /// * `height` - The height of the block to inspect.
    ///
    /// # Returns
    ///
    /// A `Result` containing the extracted data from all transactions in the block as a vector of bytes,
    /// or a `BitcoinError` if something went wrong.
    pub fn read_height(&self, height: u64) -> Result<Vec<u8>, BitcoinError> {
        // Get the block hash from the block height
        let hash = self
            .client
            .get_block_hash(height)
            .map_err(|_| BitcoinError::GetBlockErr)?;

        // Retrieve the block data using the block hash
        let block = self
            .client
            .get_block(&hash)
            .map_err(|_| BitcoinError::GetBlockErr)?;

        let mut data = Vec::new();

        // Iterate over all transactions in the block
        for tx in &block.txdata {
            // Iterate over all inputs in the transaction
            for input in &tx.input {
                // Try to get the second to last witness data
                if let Some(wit_data) = input.witness.second_to_last() {
                    // Extract data from the witness
                    if let Some(mut extracted_data) = extract_push_data(wit_data.to_vec()) {
                        data.append(&mut extracted_data);
                    }
                }
            }
        }

        Ok(data)
    }

    /// Writes provided data into Bitcoin via a taproot address.
    ///
    /// # Arguments
    ///
    /// * `data` - The data to be written to the blockchain.
    ///
    /// # Returns
    ///
    /// A `Result` containing the transaction ID of the reveal transaction or
    /// a `BitcoinError` if something went wrong.
    pub fn write(&self, data: &[u8]) -> Result<Txid, BitcoinError> {
        // Retrieve blockchain information
        let blockchain_info = self
            .client
            .get_blockchain_info()
            .map_err(|_| BitcoinError::GetBlockchainInfoErr)?;

        // Convert network name to Network type
        let network = Network::from_core_arg(&blockchain_info.chain)
            .map_err(|_| BitcoinError::InvalidNetwork)?;

        // Create data payload with protocol ID and actual data
        let mut data_with_id = Vec::from(&PROTOCOL_ID[..]);
        data_with_id.extend_from_slice(data);

        // Create a taproot address with the data included in the script
        let address = create_taproot_address(&data_with_id, network)?;

        // Commit a transaction to create the UTXO with the associated fees
        let commit_hash = self.commit_tx(&address)?;

        // Spend the UTXO, revealing the script and, consequently, the data
        let reveal_hash = self.reveal_tx(&data_with_id, &commit_hash)?;

        println!(
            "State diff written in commit_hash: {:?} and reveal_hash: {:?}",
            commit_hash, reveal_hash
        );

        Ok(reveal_hash)
    }
}

pub struct Config {
    pub host: String,
    pub user: String,
    pub pass: String,
}

impl Config {
    // Constructor to create a new Config instance
    pub fn new(host: String, user: String, pass: String) -> Self {
        Config { host, user, pass }
    }
}

/// Extracts push data from a given TapScript.
///
/// This function attempts to parse a given script, looking for a specific pattern of opcodes
/// and then extracts the data pushed after the `OP_IF` opcode until the `OP_ENDIF` opcode.
///
/// # Arguments
///
/// * `pk_script` - The script containing the data to be extracted.
///
/// # Returns
///
/// An `Option` containing the extracted data as a `Vec<u8>` if the required pattern was found,
/// otherwise `None`.
pub fn extract_push_data(pk_script: Vec<u8>) -> Option<Vec<u8>> {
    let node_info =
        NodeInfo::new_leaf_with_ver(ScriptBuf::from_bytes(pk_script), LeafVersion::TapScript);

    let tap_tree_result = TapTree::try_from(node_info);

    if let Ok(tap_tree) = tap_tree_result {
        for leaf in TapTree::script_leaves(&tap_tree) {
            let mut instructions = leaf.script().instructions();

            // Try to get the first 3 opcodes
            let op1 = instructions.next();
            let op2 = instructions.next();
            let op3 = instructions.next();

            // Check if the first opcode is `OP_FALSE`, the second is `OP_IF` and the third pushes the bytes "block"
            if matches!(
                (op1, op2, op3),
                (
                    Some(Ok(Instruction::PushBytes(opfalse))),
                    Some(Ok(Instruction::Op(opcodes::all::OP_IF))),
                    Some(Ok(Instruction::PushBytes(bytes_sequence)))
                ) if opfalse.is_empty() && bytes_sequence.as_bytes() == b"block"
            ) {
                // Skip some specific operations (assuming they are not relevant for data extraction)
                let _ = instructions.next(); // Skip _op_pushnum1
                let _ = instructions.next(); // Skip _block_number
                let _ = instructions.next(); // Skip _op_0

                // Collect the data until OP_ENDIF is found
                let mut data_collector = Vec::new();
                loop {
                    match instructions.next() {
                        Some(Ok(Instruction::Op(opcodes::all::OP_ENDIF))) => break,
                        Some(Ok(Instruction::PushBytes(data))) => {
                            data_collector.extend(data.as_bytes());
                        }
                        _ => continue,
                    }
                }

                if !data_collector.is_empty() {
                    return Some(data_collector);
                }
            }
        }
    } else {
        panic!("extract_push_data: failed to get tap tree");
    }
    None
}
