use std::str::FromStr;
use rand::prelude::SliceRandom;
use colored::*;
use solana_client::{ client_error::Result as ClientResult, rpc_config::RpcSendTransactionConfig };
use solana_program::instruction::Instruction;

use solana_rpc_client::spinner;
use solana_sdk::{
    commitment_config::CommitmentLevel,
    compute_budget::ComputeBudgetInstruction,
    pubkey::Pubkey,
    signature::{ Signature, Signer },
    system_instruction::transfer,
    transaction::Transaction,
};
use solana_transaction_status::UiTransactionEncoding;

use crate::Miner;

const RPC_RETRIES: usize = 0;
const _SIMULATION_RETRIES: usize = 4;

pub enum ComputeBudget {
    Dynamic,
    Fixed(u32),
}

impl Miner {
    pub async fn send_and_confirm(
        &self,
        ixs: &[Instruction],
        compute_budget: ComputeBudget,
        tip: u64
    ) -> ClientResult<Signature> {
        let progress_bar = spinner::new_progress_bar();
        let signer = self.signer();
        let client = self.rpc_client.clone();
        let send_client = self.send_client.clone();

        let tips = [
            "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
            "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
            "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
            "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
            "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
            "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
            "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
            "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
        ];

        // Set compute units
        let mut final_ixs = vec![];

        match compute_budget {
            ComputeBudget::Dynamic => {
                final_ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(1_400_000));
            }
            ComputeBudget::Fixed(cus) => {
                final_ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(cus));
            }
        }
        final_ixs.push(
            transfer(
                &signer.pubkey(),
                &Pubkey::from_str(
                    &tips.choose(&mut rand::thread_rng()).unwrap().to_string()
                ).unwrap(),
                tip
            )
        );
        final_ixs.push(ComputeBudgetInstruction::set_compute_unit_price(self.priority_fee));
        final_ixs.extend_from_slice(ixs);

        // Build tx
        let send_cfg = RpcSendTransactionConfig {
            skip_preflight: true,
            preflight_commitment: Some(CommitmentLevel::Confirmed),
            encoding: Some(UiTransactionEncoding::Base64),
            max_retries: Some(RPC_RETRIES),
            min_context_slot: None,
        };
        let mut tx = Transaction::new_with_payer(&final_ixs, Some(&signer.pubkey()));

        // Sign tx
        let (hash, _slot) = client
            .get_latest_blockhash_with_commitment(self.rpc_client.commitment()).await
            .unwrap();
        tx.sign(&[&signer], hash);

        loop {
            progress_bar.set_message(format!("Submitting transaction..."));
            match send_client.send_transaction_with_config(&tx, send_cfg).await {
                Ok(sig) => {
                    progress_bar.finish_with_message(format!("Sent: {}", sig));
                    return Ok(sig);
                }

                // Handle submit errors
                Err(err) => {
                    progress_bar.set_message(
                        format!("{}: {}", "ERROR".bold().red(), err.kind().to_string())
                    );
                }
            }
        }
    }
}
