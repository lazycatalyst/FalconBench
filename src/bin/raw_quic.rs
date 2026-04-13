use anyhow::{anyhow, Context, Result};
use quinn::{
    crypto::rustls::QuicClientConfig, ClientConfig, Endpoint, IdleTimeout, TransportConfig,
};
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
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
use rand::Rng;
use std::{env, net::ToSocketAddrs, str::FromStr, sync::Arc, time::Duration};

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

const FALCON_TIP_ACCOUNTS: &[&str] = &[
    "Fa1con11xLjPddfzRwRUB16sbFZggp2JeJkCeWREyR8X",
    "Fa1con11TM1RuAQzbQzYjTy4Ekfap9Lnc9fnEbQYEd6Q",
    "Fa1con113Bvi76nS5AzUiRDC2fqjfzkNMUNRLgQybMYt",
    "Fa1con1QGHJK232s8yZpzZZwqPexnAKcoyKj626LNsMv",
    "Fa1con1zUzb6qJVFz5tNkPq1Ahm8H1qKW7Q48252QbkQ",
    "Fa1con16d3MSwd3SAiwvr2LwgkpE7ot8zntbpuec8HAx",
    "Fa1con1i7mpa7Qc6epYJ6r4P9AbU77DFFz173r59Df1x",
    "Fa1con18nWn8TdAGL7JX8PertfMUGVSc899NawokJ4Bq",
    "Fa1con1GKusK2EqsfzrDzGPaYZSxQtFGzJiRMMU9Zm2g",
    "Fa1con1RDwVwM9VrJ53CwVefD3VU9c58EMpDawV7fLMi",
];

fn random_falcon_tip() -> Pubkey {
    let idx = rand::rng().random_range(0..FALCON_TIP_ACCOUNTS.len());
    pk(FALCON_TIP_ACCOUNTS[idx])
}

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

// ── Build tx ────────────────────────────────────────────────────────────────

fn build_buy_tx(keypair: &Keypair, nonce_value: Hash) -> Result<Vec<u8>> {
    let wallet = keypair.pubkey();

    let nonce_account      = env_pk("NONCE_ACCOUNT")?;
    let falcon_tip_wallet  = random_falcon_tip();
    let falcon_tip: u64    = env::var("FALCON_TIP_LAMPORTS").unwrap_or("1000000".into()).parse()?;
    let compute_price: u64 = env::var("COMPUTE_UNIT_PRICE").unwrap_or("500000".into()).parse()?;
    let buy_lamports: u64  = env::var("BUY_AMOUNT_LAMPORTS").context("BUY_AMOUNT_LAMPORTS not set")?.parse()?;
    let min_tokens_out: u64 = env::var("MIN_TOKENS_OUT").unwrap_or("1".into()).parse()?;

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

    let disc = &solana_sdk::hash::hash(b"global:buy_exact_quote_in").to_bytes()[..8];
    let mut buy_data = Vec::with_capacity(24);
    buy_data.extend_from_slice(disc);
    buy_data.extend_from_slice(&buy_lamports.to_le_bytes());
    buy_data.extend_from_slice(&min_tokens_out.to_le_bytes());

    let ixs = vec![
        system_instruction::advance_nonce_account(&nonce_account, &wallet),
        ComputeBudgetInstruction::set_compute_unit_limit(200_000),
        ComputeBudgetInstruction::set_compute_unit_price(compute_price),
        system_instruction::transfer(&wallet, &falcon_tip_wallet, falcon_tip),
        create_ata_idempotent(&wallet, &wallet, &token_mint, &t22()),
        create_ata_idempotent(&wallet, &wallet, &wsol(), &tprog()),
        system_instruction::transfer(&wallet, &user_wsol_ata, buy_lamports),
        sync_native(&user_wsol_ata),
        Instruction::new_with_bytes(pamm(), &buy_data, vec![
            AccountMeta::new(pool, false),
            AccountMeta::new(wallet, true),
            AccountMeta::new_readonly(global_cfg(), false),
            AccountMeta::new_readonly(token_mint, false),
            AccountMeta::new_readonly(wsol(), false),
            AccountMeta::new(user_token_ata, false),
            AccountMeta::new(user_wsol_ata, false),
            AccountMeta::new(pool_token_vault, false),
            AccountMeta::new(pool_sol_vault, false),
            AccountMeta::new_readonly(pool_auth, false),
            AccountMeta::new(protocol_fee_recip, false),
            AccountMeta::new_readonly(t22(), false),
            AccountMeta::new_readonly(tprog(), false),
            AccountMeta::new_readonly(sys(), false),
            AccountMeta::new_readonly(ata_prog(), false),
            AccountMeta::new_readonly(evt_auth(), false),
            AccountMeta::new_readonly(pamm(), false),
            AccountMeta::new(acct_17, false),
            AccountMeta::new_readonly(acct_18, false),
            AccountMeta::new(acct_19, false),
            AccountMeta::new(acct_20, false),
            AccountMeta::new_readonly(fee_config, false),
            AccountMeta::new_readonly(fee_prog(), false),
            AccountMeta::new_readonly(acct_23, false),
        ]),
    ];

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
    Ok(Hash::new_from_array(data[40..72].try_into()?))
}

// ── Raw QUIC client ─────────────────────────────────────────────────────────

fn make_self_signed_cert(api_key: &str) -> Result<(Vec<rustls::pki_types::CertificateDer<'static>>, rustls::pki_types::PrivateKeyDer<'static>)> {
    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
    let mut params = CertificateParams::default();
    params.distinguished_name.push(rcgen::DnType::CommonName, api_key);
    let cert = params.self_signed(&key_pair)?;

    let cert_der = rustls::pki_types::CertificateDer::from(cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(key_pair.serialize_der())
        .map_err(|e| anyhow!("key conversion: {e}"))?;

    Ok((vec![cert_der], key_der))
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let keypair = Keypair::from_base58_string(
        &env::var("PRIVATE_KEY").context("PRIVATE_KEY not set")?,
    );
    let api_key = env::var("FALCON_API_KEY").context("FALCON_API_KEY not set")?;
    let nonce_account = env_pk("NONCE_ACCOUNT")?;
    let rpc_url = env::var("RPC_URL").unwrap_or("https://api.mainnet-beta.solana.com".into());

    println!("Wallet: {}", keypair.pubkey());

    // Fetch nonce
    let rpc = RpcClient::new(rpc_url);
    let nonce_value = read_nonce_value(&rpc, &nonce_account)?;
    println!("Nonce: {nonce_value}");

    // Build tx
    let tx_bytes = build_buy_tx(&keypair, nonce_value)?;
    println!("Tx built ({} bytes)", tx_bytes.len());

    // Self-signed cert with API key as CN
    let (certs, key) = make_self_signed_cert(&api_key)?;
    println!("Cert generated (CN={})", &api_key);

    // TLS config: skip server verification (self-signed on both sides)
    let mut crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerify))
        .with_client_auth_cert(certs, key)
        .context("client auth cert setup failed")?;
    crypto.alpn_protocols = vec![b"falcon-tx".to_vec()];

    let client_crypto = QuicClientConfig::try_from(crypto)
        .map_err(|e| anyhow!("quinn crypto config: {e}"))?;
    let mut client_config = ClientConfig::new(Arc::new(client_crypto));

    let mut transport = TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(10)));
    transport.max_idle_timeout(Some(IdleTimeout::try_from(Duration::from_secs(30))?));
    client_config.transport_config(Arc::new(transport));

    let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(client_config);

    // Resolve and connect
    let addr = "fra.falcon.wtf:5000"
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow!("failed to resolve fra.falcon.wtf"))?;
    println!("Connecting to {addr}...");

    let connection = endpoint.connect(addr, "falcon")?.await?;
    println!("Connected to Falcon (raw QUIC)");

    // Send on unidirectional stream (fire-and-forget)
    let mut send = connection.open_uni().await?;
    send.write_all(&tx_bytes).await?;
    send.finish()?;
    println!("Sent ({} bytes via uni stream).", tx_bytes.len());

    // Keep alive briefly so the stream can flush
    tokio::time::sleep(Duration::from_millis(500)).await;

    endpoint.close(0u32.into(), b"done");
    println!("Done.");

    Ok(())
}

// ── Skip server certificate verification (Falcon uses self-signed) ──────────

#[derive(Debug)]
struct SkipServerVerify;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}
