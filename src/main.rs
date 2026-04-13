use anyhow::{anyhow, Context, Result};
use falcon_client::FalconClient;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::Message,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_instruction,
};
use std::{env, str::FromStr};
use uuid::Uuid;

// ── Constants ───────────────────────────────────────────────────────────────

fn pk(s: &str) -> Pubkey { Pubkey::from_str(s).unwrap() }
fn env_pk(key: &str) -> Result<Pubkey> {
    Pubkey::from_str(&env::var(key).context(format!("{key} not set"))?)
        .context(format!("invalid {key}"))
}

fn pamm()       -> Pubkey { pk("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA") }
fn global_cfg() -> Pubkey { pk("ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw") }
fn wsol()       -> Pubkey { pk("So11111111111111111111111111111111111111112") }
fn t22()        -> Pubkey { pk("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb") }
fn tprog()      -> Pubkey { pk("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA") }
fn ata_prog()   -> Pubkey { pk("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL") }
fn evt_auth()   -> Pubkey { pk("GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR") }
fn fee_prog()   -> Pubkey { pk("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ") }
fn sys()        -> Pubkey { solana_sdk::system_program::id() }

// ── Helpers ─────────────────────────────────────────────────────────────────

fn create_ata_idempotent(payer: &Pubkey, owner: &Pubkey, mint: &Pubkey, token_prog: &Pubkey) -> Instruction {
    let (ata, _) = Pubkey::find_program_address(
        &[owner.as_ref(), token_prog.as_ref(), mint.as_ref()],
        &ata_prog(),
    );
    Instruction::new_with_bytes(ata_prog(), &[1], vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(ata, false),
        AccountMeta::new_readonly(*owner, false),
        AccountMeta::new_readonly(*mint, false),
        AccountMeta::new_readonly(sys(), false),
        AccountMeta::new_readonly(*token_prog, false),
    ])
}

fn sync_native(wsol_ata: &Pubkey) -> Instruction {
    Instruction::new_with_bytes(tprog(), &[17], vec![
        AccountMeta::new(*wsol_ata, false),
    ])
}

// ── Build tx (matches reference buy tx exactly) ─────────────────────────────

fn build_buy_tx(keypair: &Keypair, nonce_value: Hash) -> Result<Vec<u8>> {
    let wallet = keypair.pubkey();

    let nonce_account  = env_pk("NONCE_ACCOUNT")?;
    let falcon_tip_wallet = env_pk("FALCON_TIP_WALLET")?;
    let falcon_tip: u64   = env::var("FALCON_TIP_LAMPORTS").unwrap_or("1000000".into()).parse()?;
    let compute_price: u64 = env::var("COMPUTE_UNIT_PRICE").unwrap_or("500000".into()).parse()?;
    let buy_lamports: u64  = env::var("BUY_AMOUNT_LAMPORTS").context("BUY_AMOUNT_LAMPORTS not set")?.parse()?;
    let min_tokens_out: u64 = env::var("MIN_TOKENS_OUT").unwrap_or("1".into()).parse()?;

    // Pool accounts
    let pool               = env_pk("POOL")?;
    let token_mint         = env_pk("TOKEN_MINT")?;
    let user_token_ata     = env_pk("USER_TOKEN_ATA")?;
    let user_wsol_ata      = env_pk("USER_WSOL_ATA")?;
    let pool_token_vault   = env_pk("POOL_TOKEN_VAULT")?;
    let pool_sol_vault     = env_pk("POOL_SOL_VAULT")?;
    let pool_auth          = env_pk("POOL_AUTH")?;
    let protocol_fee_recip = env_pk("PROTOCOL_FEE_RECIPIENT")?;
    let acct_17            = env_pk("PAMM_ACCT_17")?;
    let acct_18            = env_pk("PAMM_ACCT_18")?;
    let acct_19            = env_pk("PAMM_ACCT_19")?;
    let acct_20            = env_pk("PAMM_ACCT_20")?;
    let fee_config         = env_pk("FEE_CONFIG")?;
    let acct_23            = env_pk("PAMM_ACCT_23")?;

    // BuyExactQuoteIn discriminator: sha256("global:buy_exact_quote_in")[..8]
    let disc = &solana_sdk::hash::hash(b"global:buy_exact_quote_in").to_bytes()[..8];
    let mut buy_data = Vec::with_capacity(24);
    buy_data.extend_from_slice(disc);
    buy_data.extend_from_slice(&buy_lamports.to_le_bytes());
    buy_data.extend_from_slice(&min_tokens_out.to_le_bytes());

    let ixs = vec![
        // 0: Advance nonce
        system_instruction::advance_nonce_account(&nonce_account, &wallet),
        // 1: Compute unit limit
        ComputeBudgetInstruction::set_compute_unit_limit(200_000),
        // 2: Compute unit price
        ComputeBudgetInstruction::set_compute_unit_price(compute_price),
        // 3: Falcon tip (before swap)
        system_instruction::transfer(&wallet, &falcon_tip_wallet, falcon_tip),
        // 4: Create user token ATA (Token-2022, idempotent)
        create_ata_idempotent(&wallet, &wallet, &token_mint, &t22()),
        // 5: Create user WSOL ATA (idempotent)
        create_ata_idempotent(&wallet, &wallet, &wsol(), &tprog()),
        // 6: Transfer SOL → WSOL ATA (wrap for buy)
        system_instruction::transfer(&wallet, &user_wsol_ata, buy_lamports),
        // 7: SyncNative
        sync_native(&user_wsol_ata),
        // 8: pAMM BuyExactQuoteIn (24 accounts)
        Instruction::new_with_bytes(pamm(), &buy_data, vec![
            AccountMeta::new(pool, false),                      // 0
            AccountMeta::new(wallet, true),                     // 1 signer
            AccountMeta::new_readonly(global_cfg(), false),     // 2
            AccountMeta::new_readonly(token_mint, false),       // 3
            AccountMeta::new_readonly(wsol(), false),           // 4
            AccountMeta::new(user_token_ata, false),            // 5
            AccountMeta::new(user_wsol_ata, false),             // 6
            AccountMeta::new(pool_token_vault, false),          // 7
            AccountMeta::new(pool_sol_vault, false),            // 8
            AccountMeta::new_readonly(pool_auth, false),        // 9
            AccountMeta::new(protocol_fee_recip, false),        // 10
            AccountMeta::new_readonly(t22(), false),            // 11
            AccountMeta::new_readonly(tprog(), false),          // 12
            AccountMeta::new_readonly(sys(), false),            // 13
            AccountMeta::new_readonly(ata_prog(), false),       // 14
            AccountMeta::new_readonly(evt_auth(), false),       // 15
            AccountMeta::new_readonly(pamm(), false),           // 16
            AccountMeta::new(acct_17, false),                   // 17
            AccountMeta::new_readonly(acct_18, false),          // 18
            AccountMeta::new(acct_19, false),                   // 19
            AccountMeta::new(acct_20, false),                   // 20
            AccountMeta::new_readonly(fee_config, false),       // 21
            AccountMeta::new_readonly(fee_prog(), false),       // 22
            AccountMeta::new_readonly(acct_23, false),          // 23
        ]),
    ];

    // Sign with nonce value
    let mut msg = Message::new(&ixs, Some(&wallet));
    msg.recent_blockhash = nonce_value;
    let msg_bytes = msg.serialize();
    let sig = keypair.sign_message(&msg_bytes);

    let mut wire = Vec::with_capacity(1 + 64 + msg_bytes.len());
    wire.push(1u8);
    wire.extend_from_slice(sig.as_ref());
    wire.extend_from_slice(&msg_bytes);

    if wire.len() > 1232 {
        return Err(anyhow!("tx too large: {} bytes (max 1232)", wire.len()));
    }
    Ok(wire)
}

fn read_nonce_value(rpc: &RpcClient, nonce_account: &Pubkey) -> Result<Hash> {
    let data = rpc.get_account_data(nonce_account)
        .context("failed to read nonce account")?;
    if data.len() < 72 {
        return Err(anyhow!("nonce account data too short: {} bytes", data.len()));
    }
    // Layout: [4 version][4 state][32 authority][32 nonce_hash]...
    Ok(Hash::new_from_array(data[40..72].try_into()?))
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let keypair = Keypair::from_base58_string(
        &env::var("PRIVATE_KEY").context("PRIVATE_KEY not set")?,
    );
    let api_key = Uuid::parse_str(
        &env::var("FALCON_API_KEY").context("FALCON_API_KEY not set")?,
    )?;
    let nonce_account = env_pk("NONCE_ACCOUNT")?;
    let rpc_url = env::var("RPC_URL").unwrap_or("https://api.mainnet-beta.solana.com".into());

    println!("Wallet: {}", keypair.pubkey());

    // Single RPC call: read current nonce value
    let rpc = RpcClient::new(rpc_url);
    let nonce_value = read_nonce_value(&rpc, &nonce_account)?;
    println!("Nonce: {nonce_value}");

    let tx_bytes = build_buy_tx(&keypair, nonce_value)?;
    println!("Tx built ({} bytes)", tx_bytes.len());

    let mut client = FalconClient::connect("fra.falcon.wtf:5000", api_key).await?;
    println!("Connected to Falcon");

    client.set_send_timeout(std::time::Duration::from_secs(5));
    client.send_transaction_payload(&tx_bytes).await?;
    println!("Sent.");

    Ok(())
}
