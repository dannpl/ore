use colored::*;
use drillx::{
    equix::{self},
    Hash, Solution,
};
use futures::StreamExt;
use ore_api::{
    consts::{BUS_ADDRESSES, BUS_COUNT},
    state::Proof,
};
use rand::SeedableRng;
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};
use solana_program::pubkey::Pubkey;
use solana_rpc_client::spinner;
use solana_sdk::signer::Signer;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use tokio::sync::RwLock;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::{
    args::MineArgs,
    send_and_confirm::ComputeBudget,
    utils::{amount_u64_to_string, get_clock, get_proof_with_authority, proof_pubkey},
    Miner,
};

#[derive(Debug, Deserialize, Serialize)]
pub struct Tip {
    pub time: String,
    pub landed_tips_25th_percentile: f64,
    pub landed_tips_50th_percentile: f64,
    pub landed_tips_75th_percentile: f64,
    pub landed_tips_95th_percentile: f64,
    pub landed_tips_99th_percentile: f64,
    pub ema_landed_tips_50th_percentile: f64,
}

impl Miner {
    pub async fn mine(&self, args: MineArgs) {
        // Register, if needed.
        let signer = self.signer();
        self.open().await;

        let tip = Arc::new(RwLock::new(0_u64));
        let tip_clone = Arc::clone(&tip);
        let mut current_tip = 0;

        // Check num threads
        self.check_num_cores(args.threads);

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

        // Start mining loop
        loop {
            let proof = get_proof_with_authority(&self.rpc_client, signer.pubkey()).await;

            println!(
                "\nStake balance: {} ORE",
                amount_u64_to_string(proof.balance).green()
            );

            let cutoff_time = self.get_cutoff(proof).await;

            let mut cutt = cutoff_time;

            while cutt > 0 {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                cutt -= 1;

                println!("Mining in {} sec", cutt.to_string().bold().green(),);
            }

            let solution = Self::find_hash_par(proof, args.threads, args.diff as u32);

            let compute_budget = 500_000;

            current_tip = *tip.read().await;

            let mut ixs = vec![];

            ixs.push(ore_api::instruction::auth(proof_pubkey(signer.pubkey())));

            ixs.push(ore_api::instruction::mine(
                signer.pubkey(),
                signer.pubkey(),
                find_bus(),
                solution,
            ));

            self.send_and_confirm(
                &ixs,
                ComputeBudget::Fixed(compute_budget),
                current_tip.clone(),
            )
            .await
            .ok();

            tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
        }
    }

    fn find_hash_par(proof: Proof, num_threads: u64, min_difficulty: u32) -> Solution {
        let proof = Arc::new(proof);
        let progress_bar = Arc::new(spinner::new_progress_bar());

        let (tx, rx) = mpsc::channel();

        let best_hash = Arc::new(Mutex::new(Hash::default()));
        let best_difficulty = Arc::new(Mutex::new(0));
        let best_nonce = Arc::new(Mutex::new(0u64));
        let found_solution = Arc::new(Mutex::new(false));

        for thread_id in 0..num_threads {
            let proof = Arc::clone(&proof);
            let tx = tx.clone();
            let best_hash = Arc::clone(&best_hash);
            let best_difficulty = Arc::clone(&best_difficulty);
            let best_nonce = Arc::clone(&best_nonce);
            let found_solution = Arc::clone(&found_solution);
            let mut memory = equix::SolverMemory::new();

            thread::spawn(move || loop {
                if *found_solution.lock().unwrap() {
                    break;
                }

                let thread_seed = rand::thread_rng().next_u64() ^ thread_id * 3.69 as u64;
                let mut rng = rand::rngs::StdRng::seed_from_u64(thread_seed);

                let nonce = rng.gen::<u64>();

                if let Ok(hx) =
                    drillx::hash_with_memory(&mut memory, &proof.challenge, &nonce.to_le_bytes())
                {
                    let difficulty = hx.difficulty();

                    let mut best_diff = best_difficulty.lock().unwrap();

                    if difficulty > *best_diff {
                        println!("Difficulty: {}", format!("{:?}", difficulty).bold().green());

                        *best_diff = difficulty;

                        let mut best_h = best_hash.lock().unwrap();
                        *best_h = hx;

                        let mut best_n = best_nonce.lock().unwrap();
                        *best_n = nonce;

                        if difficulty >= min_difficulty {
                            println!("Solution found by thread {}", thread_id);
                            *found_solution.lock().unwrap() = true;

                            let _ = tx.send(());

                            break;
                        }
                    }
                }
            });
        }

        rx.recv().unwrap();

        progress_bar.finish_with_message("Mining completed");

        let best_hash = best_hash.lock().unwrap();
        let best_nonce = best_nonce.lock().unwrap();

        Solution::new(best_hash.d, best_nonce.to_le_bytes())
    }

    pub fn check_num_cores(&self, threads: u64) {
        // Check num threads
        let num_cores = num_cpus::get() as u64;
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
}

// TODO Pick a better strategy (avoid draining bus)
fn find_bus() -> Pubkey {
    let i = rand::thread_rng().gen_range(0..BUS_COUNT);
    BUS_ADDRESSES[i]
}
