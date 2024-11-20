use core::str;

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::account_info::next_account_info;
use solana_program::program::invoke;
use solana_program::rent::Rent;
use solana_program::sysvar::Sysvar as _;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg, program_error::ProgramError,
    pubkey::Pubkey, sysvar::clock::Clock,
};
use solana_program::{entrypoint, system_instruction};

const SECRET_LENGTH: usize = 256;

entrypoint!(process_instruction);

fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    match TimeLockInstruction::unpack(instruction_data)? {
        TimeLockInstruction::InitializeTimeLock { timestamp, secret } => {
            msg!("Instruction: InitializeTimeLock");
            initialize_time_lock(program_id, accounts, timestamp, secret)?;
        }
        TimeLockInstruction::TryUnlock => try_unlock(program_id, accounts)?,
    }
    Ok(())
}

// Define struct representing our time lock account's data
#[derive(BorshSerialize, BorshDeserialize, Debug)]
pub struct TimeLockAccount {
    timestamp: i64,
    secret: [u8; SECRET_LENGTH],
}

#[allow(clippy::large_enum_variant)]
#[derive(BorshSerialize, BorshDeserialize, Debug)]
pub enum TimeLockInstruction {
    InitializeTimeLock {
        timestamp: i64,
        /// encrypted encoded secret
        secret: [u8; SECRET_LENGTH],
    },
    TryUnlock,
}

impl TimeLockInstruction {
    pub fn unpack(input: &[u8]) -> Result<Self, ProgramError> {
        let (tag, rest) = input
            .split_first()
            .ok_or(ProgramError::InvalidInstructionData)?;
        match tag {
            0 => {
                let (timestamp, rest) = rest.split_at(8);
                let timestamp = i64::from_le_bytes(
                    timestamp
                        .try_into()
                        .map_err(|_| ProgramError::InvalidInstructionData)?,
                );
                let secret: [u8; SECRET_LENGTH] = rest
                    .try_into()
                    .map_err(|_| ProgramError::InvalidInstructionData)?;
                // check that the secret is valid utf8
                str::from_utf8(&secret).map_err(|_| ProgramError::InvalidAccountData)?;
                Ok(Self::InitializeTimeLock { timestamp, secret })
            }
            1 => Ok(Self::TryUnlock),
            _ => Err(ProgramError::InvalidInstructionData),
        }
    }
}

fn initialize_time_lock(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    timestamp: i64,
    secret: [u8; SECRET_LENGTH],
) -> ProgramResult {
    let now = Clock::get()?.unix_timestamp;
    if now >= timestamp {
        return Err(ProgramError::InvalidInstructionData);
    }

    let accounts_iter = &mut accounts.iter();

    let timelock_data_account = next_account_info(accounts_iter)?;
    let payer_account = next_account_info(accounts_iter)?;
    let system_program = next_account_info(accounts_iter)?;

    // Size of our timelock data
    let account_space = 8 + SECRET_LENGTH; // i64 timestamp + u64 secret length + SECRET_LENGTH byte secret

    // Calculate minimum balance for rent exemption
    let rent = Rent::get()?;
    let required_lamports = rent.minimum_balance(account_space);

    // Create the timelock account
    invoke(
        &system_instruction::create_account(
            payer_account.key,         // Account paying for the new account
            timelock_data_account.key, // Account to be created
            required_lamports,         // Amount of lamports to transfer to the new account
            account_space as u64,      // Size in bytes to allocate for the data field
            program_id,                // Set program owner to our program
        ),
        &[
            payer_account.clone(),
            timelock_data_account.clone(),
            system_program.clone(),
        ],
    )?;

    // Create a new TimeLockAccount struct with the initial value
    let timelock_data = TimeLockAccount { timestamp, secret };

    // Get a mutable reference to the timelock account's data
    let mut account_data = &mut timelock_data_account.data.borrow_mut()[..];

    // Serialize the TimeLockAccount struct into the account's data
    timelock_data.serialize(&mut account_data)?;

    msg!("TimeLock set to unix timestamp: {}", timestamp);
    Ok(())
}

fn try_unlock(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();
    let timelock_data_account = next_account_info(accounts_iter)?;

    // Verify account ownership
    if timelock_data_account.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }

    // Deserialize the account data
    let timelock_data = TimeLockAccount::try_from_slice(&timelock_data_account.data.borrow())?;
    let now = Clock::get()?.unix_timestamp;

    match now >= timelock_data.timestamp {
        true => msg!(
            "TimeLock unlocked! Encryped secret: {}",
            str::from_utf8(&timelock_data.secret).map_err(|_| ProgramError::InvalidAccountData)?
        ),
        false => msg!("TimeLock will lock until {}", timelock_data.timestamp),
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use solana_program_test::*;
    use solana_sdk::{
        instruction::{AccountMeta, Instruction},
        signature::{Keypair, Signer},
        system_program,
        transaction::Transaction,
    };

    #[tokio::test]
    async fn test_timelock_program() {
        let program_id = Pubkey::new_unique();
        let (banks_client, payer, recent_blockhash) = ProgramTest::new(
            "timelock_program",
            program_id,
            processor!(process_instruction),
        )
        .start()
        .await;

        // Create a new keypair to use as the address for our timelock account
        let timelock_keypair = Keypair::new();
        let timestamp: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 5;
        let secret = [65; SECRET_LENGTH];

        // Step 1: Initialize the timelock
        println!("Testing timelock initialization...");

        // Create initialization instruction
        let mut init_instruction_data = vec![0]; // 0 = initialize instruction
        init_instruction_data.extend_from_slice(&timestamp.to_le_bytes());
        init_instruction_data.extend_from_slice(&secret);

        let initialize_instruction = Instruction::new_with_bytes(
            program_id,
            &init_instruction_data,
            vec![
                AccountMeta::new(timelock_keypair.pubkey(), true),
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
        );

        // Send transaction with initialize instruction
        let mut transaction =
            Transaction::new_with_payer(&[initialize_instruction], Some(&payer.pubkey()));
        transaction.sign(&[&payer, &timelock_keypair], recent_blockhash);
        banks_client.process_transaction(transaction).await.unwrap();

        // Check account data
        let account = banks_client
            .get_account(timelock_keypair.pubkey())
            .await
            .expect("Failed to get timelock account");

        if let Some(account_data) = account {
            let timelock = TimeLockAccount::try_from_slice(&account_data.data)
                .expect("Failed to deserialize timelock data");
            assert_eq!(timelock.timestamp, timestamp);
            println!(
                "✅ TimeLock initialized successfully with value: {}",
                timelock.timestamp
            );
        }

        // Step 2: Increment the timelock
        println!("Testing timelock unlock...");

        // Create increment instruction
        let increment_instruction = Instruction::new_with_bytes(
            program_id,
            &[1], // 1 = try unlock instruction
            vec![AccountMeta::new(timelock_keypair.pubkey(), true)],
        );

        // Send transaction with increment instruction
        let mut transaction =
            Transaction::new_with_payer(&[increment_instruction], Some(&payer.pubkey()));
        transaction.sign(&[&payer, &timelock_keypair], recent_blockhash);
        banks_client.process_transaction(transaction).await.unwrap();

        // Check account data
        let account = banks_client
            .get_account(timelock_keypair.pubkey())
            .await
            .expect("Failed to get timelock account");

        if let Some(account_data) = account {
            let timelock = TimeLockAccount::try_from_slice(&account_data.data)
                .expect("Failed to deserialize timelock data");
            assert_eq!(timelock.secret, secret);
            println!(
                "✅ TimeLock unlock successfully: {}",
                str::from_utf8(&timelock.secret).unwrap()
            );
        }
    }
}
