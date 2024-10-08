use colored::*;
use drillx::{equix, Hash, Solution};
use futures::StreamExt;
use ore_api::{
    consts::{BUS_ADDRESSES, BUS_COUNT},
    state::{Bus, Proof},
};
use ore_utils::AccountDeserialize;
use rand::Rng;
use rayon::prelude::*;
use solana_program::pubkey::Pubkey;
use solana_rpc_client::spinner;
use solana_sdk::signer::Signer;
use std::sync::atomic::AtomicU32;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::{
    args::MineArgs,
    send_and_confirm::ComputeBudget,
    utils::{
        amount_u64_to_string, get_clock, get_config, get_proof_with_authority, proof_pubkey, Tip,
    },
    Miner,
};
use crossbeam::channel;

impl Miner {
    pub async fn mine(&self, args: MineArgs) {
        let signer = self.signer();

        self.open().await;

        let tip = Arc::new(RwLock::new(0_u64));
        let tip_clone = Arc::clone(&tip);

        if self.jito {
            let url = "ws://bundles-api-rest.jito.wtf/api/v1/bundles/tip_stream";
            let (ws_stream, _) = connect_async(url).await.unwrap();
            let (_, mut read) = ws_stream.split();

            tokio::spawn(async move {
                while let Some(message) = read.next().await {
                    if let Ok(Message::Text(text)) = message {
                        if let Ok(tips) = serde_json::from_str::<Vec<Tip>>(&text) {
                            for item in tips {
                                let mut tip = tip_clone.write().await;
                                *tip =
                                    (item.landed_tips_50th_percentile * (10_f64).powf(9.0)) as u64;
                            }
                        }
                    }
                }
            });
        }

        let core_ids = core_affinity::get_core_ids().unwrap();

        println!(
            "{}",
            format!("Mining with {} threads", core_ids.len())
                .bold()
                .green()
        );

        loop {
            let proof = get_proof_with_authority(&self.rpc_client, signer.pubkey()).await;

            let config = get_config(&self.rpc_client).await;

            println!(
                "\nHashes: {} Rewards: {}",
                proof.total_hashes.to_string().bold().blue(),
                (proof.total_rewards as f64 / 10u64.pow(11) as f64)
                    .to_string()
                    .bold()
                    .blue()
            );

            println!(
                "\nStake: {} ORE Multiplier: {}x",
                amount_u64_to_string(proof.balance).bold().green(),
                format!(
                    "{:?}",
                    calculate_multiplier(proof.balance, config.top_balance)
                )
                .bold()
                .green()
            );

            let solution = Self::find_hash_par(&self, proof, args.diff).await;

            let mut ixs = vec![];

            let current_tip = *tip.read().await;

            ixs.push(ore_api::instruction::auth(proof_pubkey(signer.pubkey())));

            ixs.push(ore_api::instruction::mine(
                signer.pubkey(),
                signer.pubkey(),
                self.find_bus().await,
                solution,
            ));

            self.send_and_confirm(&ixs, ComputeBudget::Fixed(500_000), current_tip.clone())
                .await
                .ok();

            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    }

    async fn find_hash_par(&self, proof: Proof, min_difficulty: u32) -> Solution {
        let progress_bar = Arc::new(spinner::new_progress_bar());
        let best_difficulty = Arc::new(AtomicU32::new(0));
        let best_nonce = Arc::new(AtomicU64::new(0));
        let best_hash = Arc::new(Mutex::new(Hash::default()));
        let (sender, _receiver) = channel::unbounded();
        let timeout = Duration::from_secs(45);
        let start_time = Instant::now();
        let rt = tokio::runtime::Handle::current();
        let core_ids = core_affinity::get_core_ids().unwrap();

        let handles: Vec<_> = core_ids
            .into_par_iter()
            .map(|core_id| {
                let proof = proof.clone();
                let best_difficulty = Arc::clone(&best_difficulty);
                let best_nonce = Arc::clone(&best_nonce);
                let best_hash = Arc::clone(&best_hash);
                let progress_bar = Arc::clone(&progress_bar);

                let sender = sender.clone();

                let mut memory = equix::SolverMemory::new();
                rt.spawn_blocking(move || loop {
                    if best_difficulty.load(Ordering::Relaxed) >= min_difficulty
                        || start_time.elapsed() > timeout
                    {
                        let _ = sender.send(());
                        break;
                    }

                    let mut rng = rand::thread_rng();

                    let nonce: u64 = core_id.id as u64 * rng.gen_range(0..u64::MAX);

                    if let Ok(hx) = drillx::hash_with_memory(
                        &mut memory,
                        &proof.challenge,
                        &nonce.to_le_bytes(),
                    ) {
                        let difficulty = hx.difficulty();
                        let current_best = best_difficulty.load(Ordering::Relaxed);

                        if difficulty > current_best {
                            best_nonce.store(nonce, Ordering::Relaxed);
                            best_difficulty.store(difficulty, Ordering::Relaxed);

                            let mut bh = best_hash.lock().unwrap();
                            *bh = hx;

                            progress_bar.set_message(format!(
                                "Difficulty: {}",
                                format!("{:?}", difficulty).bold().green()
                            ));
                        }
                    }
                })
            })
            .collect();

        for handle in handles {
            if let Err(err) = handle.await {
                eprintln!("Thread error: {:?}", err);
            }
        }

        let final_best_hash = best_hash.lock().unwrap();
        let final_best_nonce = best_nonce.load(Ordering::Relaxed);
        let final_best_difficulty = best_difficulty.load(Ordering::Relaxed);

        let cutoff_time = self.get_cutoff(proof).await;

        let mut cutt = cutoff_time;

        while cutt > 0 {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            cutt -= 1;

            progress_bar.set_message(format!(
                "Best Difficult: {} - Waiting {} sec to send",
                format!("{:?}", best_difficulty).bold().green(),
                format!("{:?}", cutt).bold().green()
            ))
        }

        if final_best_difficulty < min_difficulty {
            println!(
                "{}",
                format!("The min difficulty not reached: {}", min_difficulty)
                    .bold()
                    .red()
            );
        }

        progress_bar.finish_with_message(format!(
            "Best hash: {} (difficulty: {})",
            bs58::encode(final_best_hash.h).into_string(),
            final_best_difficulty
        ));

        Solution::new(final_best_hash.d, final_best_nonce.to_le_bytes())
    }

    pub fn check_num_cores(&self, threads: u64) {
        let num_cores = std::thread::available_parallelism().unwrap().get() as u64;
        if threads.gt(&num_cores) {
            println!(
                "{} Number of threads ({}) exceeds available cores ({})",
                "WARNING".bold().yellow(),
                threads,
                num_cores
            );
        }
    }

    async fn get_cutoff(&self, proof: Proof) -> u64 {
        let clock = get_clock(&self.rpc_client).await;
        proof
            .last_hash_at
            .saturating_add(60)
            .saturating_sub(0)
            .saturating_sub(clock.unix_timestamp)
            .max(0) as u64
    }

    async fn find_bus(&self) -> Pubkey {
        // Fetch the bus with the largest balance
        if let Ok(accounts) = self.rpc_client.get_multiple_accounts(&BUS_ADDRESSES).await {
            let mut top_bus_balance: u64 = 0;
            let mut top_bus = BUS_ADDRESSES[0];
            for account in accounts {
                if let Some(account) = account {
                    if let Ok(bus) = Bus::try_from_bytes(&account.data) {
                        if bus.rewards.gt(&top_bus_balance) {
                            top_bus_balance = bus.rewards;
                            top_bus = BUS_ADDRESSES[bus.id as usize];
                        }
                    }
                }
            }
            return top_bus;
        }

        // Otherwise return a random bus
        let i = rand::thread_rng().gen_range(0..BUS_COUNT);
        BUS_ADDRESSES[i]
    }
}

fn calculate_multiplier(balance: u64, top_balance: u64) -> f64 {
    1.0 + (balance as f64 / top_balance as f64).min(1.0f64)
}
