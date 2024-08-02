use std::str::FromStr;

use ore_api::consts::MINT_ADDRESS;
use solana_sdk::{ pubkey::Pubkey, signer::Signer };

use crate::{
    cu_limits::CU_LIMIT_UPGRADE,
    send_and_confirm::ComputeBudget,
    utils::amount_f64_to_u64_v1,
    Miner,
    UpgradeArgs,
    DEFAULT_JITO_TIP,
};

impl Miner {
    pub async fn upgrade(&self, args: UpgradeArgs) {
        let signer = &self.signer();
        let beneficiary = self.get_or_initialize_ata().await;
        let (sender, sender_balance) = self.get_ata_v1().await;

        println!("{}", MINT_ADDRESS);

        let amount_f64 = match args.amount {
            Some(f64) => f64,
            None => {
                println!("Defaulting to max amount of v1 Ore token in wallet: {}", sender_balance);
                sender_balance
            }
        };
        let amount = amount_f64_to_u64_v1(amount_f64);

        let ix = ore_api::instruction::upgrade(signer.pubkey(), beneficiary, sender, amount);
        match
            self.send_and_confirm(
                &[ix],
                ComputeBudget::Fixed(CU_LIMIT_UPGRADE),
                DEFAULT_JITO_TIP
            ).await
        {
            Ok(_sig) => {}
            Err(err) => {
                println!("error: {}", err);
            }
        }
    }

    // asserts that token account exists and gets balance
    async fn get_ata_v1(&self) -> (Pubkey, f64) {
        // Initialize client.
        let signer = self.signer();
        let client = self.rpc_client.clone();

        // Derive assoicated token address (for v1 account)
        let token_account_pubkey_v1 = spl_associated_token_account::get_associated_token_address(
            &signer.pubkey(),
            &ore_api::consts::MINT_V1_ADDRESS
        );

        // Get token account balance
        let balance = match client.get_token_account(&token_account_pubkey_v1).await {
            Ok(None) => { panic!("v1 token account doesn't exist") }
            Ok(Some(token_account)) =>
                match token_account.token_amount.ui_amount {
                    Some(ui_amount) => ui_amount,
                    None => {
                        panic!(
                            "Error parsing token account UI amount: {}",
                            token_account.token_amount.amount
                        )
                    }
                }
            Err(err) => { panic!("Error fetching token account: {}", err) }
        };

        // Return v1 token account address
        (token_account_pubkey_v1, balance)
    }

    async fn get_or_initialize_ata(&self) -> Pubkey {
        // Initialize client
        let signer = self.signer();
        let client = self.rpc_client.clone();

        // Derive assoicated token address (ata)
        let token_account_pubkey = spl_associated_token_account::get_associated_token_address(
            &signer.pubkey(),
            &Pubkey::from_str("oreoU2P8bN6jkk3jbaiVxYnG1dCXcYxwhwyK9jSybcp").unwrap()
        );

        // Check if ata already exists or init
        if let Err(_err) = client.get_token_account(&token_account_pubkey).await {
            println!("Initializing v2 token account...");
            let ix = spl_associated_token_account::instruction::create_associated_token_account(
                &signer.pubkey(),
                &signer.pubkey(),
                &Pubkey::from_str("oreoU2P8bN6jkk3jbaiVxYnG1dCXcYxwhwyK9jSybcp").unwrap(),
                &spl_token::id()
            );
            self.send_and_confirm(&[ix], ComputeBudget::Dynamic, DEFAULT_JITO_TIP).await.ok();
        }

        // Return token account address
        token_account_pubkey
    }
}
