use ckb_testtool::{
    builtin::ALWAYS_SUCCESS,
    ckb_types::{
        bytes::Bytes,
        core::{
            EpochNumberWithFraction, HeaderBuilder, ScriptHashType, TransactionBuilder,
            TransactionView,
        },
        packed::{
            Byte32, CellDep, CellInput, CellOutput, OutPoint, RawTransaction, Script, WitnessArgs,
        },
        prelude::*,
    },
    context::Context,
};
use merkle_cbt::CBMT;
use tally_builder::{TallyBuilder, prove_block_transactions, prove_transaction};
use treasury_common::{
    BatchWitness, MergeHash, ProposalConfig, ProposalData, ProposalPhase, ResultData, TallyPhase,
    TallyState, TallyWitness, TreasuryConfig, VoteData, blake2b_256, hash_pair, transactions_root,
};

const CKB: u64 = 100_000_000;

fn proposal(_proposal_id: [u8; 32], _vote_code_hash: [u8; 32]) -> ProposalData {
    ProposalData {
        phase: ProposalPhase::Closed,
        start_block: 10,
        end_block: 11,
        challenge_period: 5,
        max_events_per_batch: 100,
        max_dao_deps_per_vote: 64,
        max_state_keys_per_batch: 4096,
        max_batch_sequence: 128,
        max_batch_witness_bytes: 500_000,
        minimum_vote_capacity: 100 * CKB,
        requested_amount: 1_000 * CKB,
        receiver_lock_hash: [1; 32],
        proposal_config_type_hash: [6; 32],
        metadata_hash: [5; 32],
    }
}

fn type_id(first_input: &CellInput, output_index: u64) -> [u8; 32] {
    let mut preimage = first_input.as_slice().to_vec();
    preimage.extend_from_slice(&output_index.to_le_bytes());
    blake2b_256(&preimage)
}

fn tally_proposal_config(
    proposal_type: &Script,
    vote_code_hash: [u8; 32],
    tally_type: &Script,
    candidate_lock: &Script,
) -> ProposalConfig {
    ProposalConfig {
        approval_bps: 6_000,
        minimum_total_votes: 1,
        maximum_proposal_amount: 1_000 * CKB,
        minimum_challenge_period: 5,
        treasury_lock_hash: [7; 32],
        dao_code_hash: [8; 32],
        dao_hash_type: 1,
        proposal_code_hash: proposal_type.code_hash().as_slice().try_into().unwrap(),
        proposal_hash_type: proposal_type.hash_type().as_slice()[0],
        vote_code_hash,
        vote_hash_type: 1,
        tally_code_hash: tally_type.code_hash().as_slice().try_into().unwrap(),
        tally_hash_type: tally_type.hash_type().as_slice()[0],
        candidate_lock_hash: candidate_lock
            .calc_script_hash()
            .as_slice()
            .try_into()
            .unwrap(),
        policy_type_hash: [9; 32],
    }
}

fn create_proposal_config_dep(
    context: &mut Context,
    always_success: &OutPoint,
    lock: &Script,
    config: ProposalConfig,
) -> (OutPoint, [u8; 32]) {
    let config_type = context
        .build_script(always_success, Bytes::from(vec![0xc0]))
        .unwrap();
    let config_hash = config_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    let cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(lock.clone())
            .type_(Some(config_type).pack())
            .build(),
        Bytes::from(config.encode()),
    );
    (cell, config_hash)
}

#[test]
fn proposal_contract_uses_proposal_config_as_identity_source() {
    let mut context = Context::default();
    let proposal_code = context.deploy_cell_by_name("proposal-type-script");
    let always_success = context.deploy_cell(ALWAYS_SUCCESS.clone());
    let owner_lock = context
        .build_script(&always_success, Bytes::from(vec![1]))
        .unwrap();
    let config_type = context
        .build_script(&always_success, Bytes::from(vec![2]))
        .unwrap();
    let proposal_identity = context.build_script(&proposal_code, Bytes::new()).unwrap();
    let config = ProposalConfig {
        approval_bps: 6_000,
        minimum_total_votes: 100 * CKB as u128,
        maximum_proposal_amount: 1_000 * CKB,
        minimum_challenge_period: 5,
        treasury_lock_hash: [7; 32],
        dao_code_hash: [8; 32],
        dao_hash_type: 1,
        proposal_code_hash: proposal_identity.code_hash().as_slice().try_into().unwrap(),
        proposal_hash_type: proposal_identity.hash_type().as_slice()[0],
        vote_code_hash: [4; 32],
        vote_hash_type: 1,
        tally_code_hash: [3; 32],
        tally_hash_type: 2,
        candidate_lock_hash: [14; 32],
        policy_type_hash: [4; 32],
    };
    let config_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(owner_lock.clone())
            .type_(Some(config_type.clone()).pack())
            .build(),
        Bytes::from(config.encode()),
    );
    let funding_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(owner_lock.clone())
            .build(),
        Bytes::new(),
    );
    let funding_input = CellInput::new_builder()
        .previous_output(funding_cell)
        .build();
    let proposal_type = context
        .build_script(
            &proposal_code,
            Bytes::from(type_id(&funding_input, 0).to_vec()),
        )
        .unwrap();
    let proposal_id = proposal_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    let mut proposal = proposal(proposal_id, [4; 32]);
    proposal.phase = ProposalPhase::Open;
    proposal.proposal_config_type_hash = config_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();

    let build = |config_cell: OutPoint, data: Vec<u8>| {
        TransactionBuilder::default()
            .cell_dep(CellDep::new_builder().out_point(config_cell).build())
            .input(funding_input.clone())
            .output(
                CellOutput::new_builder()
                    .capacity(1_000 * CKB)
                    .lock(owner_lock.clone())
                    .type_(Some(proposal_type.clone()).pack())
                    .build(),
            )
            .output_data(Bytes::from(data).pack())
            .build()
    };
    let valid = context.complete_tx(build(config_cell.clone(), proposal.encode()));
    context.verify_tx(&valid, 20_000_000).unwrap();

    proposal.challenge_period = config.minimum_challenge_period - 1;
    let short_challenge = context.complete_tx(build(config_cell.clone(), proposal.encode()));
    assert!(context.verify_tx(&short_challenge, 20_000_000).is_err());
    proposal.challenge_period = config.minimum_challenge_period;

    let valid_config_hash = proposal.proposal_config_type_hash;
    proposal.proposal_config_type_hash = [0x99; 32];
    let missing_config = context.complete_tx(build(config_cell, proposal.encode()));
    assert!(context.verify_tx(&missing_config, 20_000_000).is_err());

    let wrong_config_type = context
        .build_script(&always_success, Bytes::from(vec![5]))
        .unwrap();
    let mut wrong_config = config;
    wrong_config.proposal_code_hash = [0x98; 32];
    let wrong_config_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(owner_lock.clone())
            .type_(Some(wrong_config_type.clone()).pack())
            .build(),
        Bytes::from(wrong_config.encode()),
    );
    proposal.proposal_config_type_hash = wrong_config_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    assert_ne!(proposal.proposal_config_type_hash, valid_config_hash);
    let unauthorized = context.complete_tx(build(wrong_config_cell, proposal.encode()));
    assert!(context.verify_tx(&unauthorized, 20_000_000).is_err());
}

fn historical_vote_raw(
    proposal_id: [u8; 32],
    vote_code_hash: [u8; 32],
    voter_lock: Script,
    version: u32,
) -> RawTransaction {
    let dao_out_point = OutPoint::new_builder()
        .tx_hash(Byte32::from_slice(&[0x44; 32]).unwrap())
        .index(1u32)
        .build();
    let vote_type = Script::new_builder()
        .code_hash(Byte32::from_slice(&vote_code_hash).unwrap())
        .hash_type(ScriptHashType::Type)
        .args(Bytes::from(proposal_id.to_vec()).pack())
        .build();
    let vote_data = VoteData {
        direction: 1,
        amount: 500 * CKB,
        dao_dep_indices: vec![0],
    }
    .encode()
    .unwrap();
    let cell_dep = CellDep::new_builder().out_point(dao_out_point).build();
    let output = CellOutput::new_builder()
        .capacity(500 * CKB)
        .lock(voter_lock)
        .type_(Some(vote_type).pack())
        .build();
    RawTransaction::new_builder()
        .version(version)
        .cell_deps(vec![cell_dep].pack())
        .outputs(vec![output].pack())
        .outputs_data([Bytes::from(vote_data)].pack())
        .build()
}

fn historical_dao_spend_raw() -> RawTransaction {
    let dao_out_point = OutPoint::new_builder()
        .tx_hash(Byte32::from_slice(&[0x44; 32]).unwrap())
        .index(1u32)
        .build();
    RawTransaction::new_builder()
        .inputs(
            [CellInput::new_builder()
                .previous_output(dao_out_point)
                .build()]
            .pack(),
        )
        .build()
}

fn historical_unique_vote_raw(
    proposal_id: [u8; 32],
    vote_code_hash: [u8; 32],
    voter_lock: Script,
    voter: u32,
) -> RawTransaction {
    let mut dao_tx_hash = [0x44; 32];
    dao_tx_hash[..4].copy_from_slice(&voter.to_le_bytes());
    let dao_out_point = OutPoint::new_builder()
        .tx_hash(Byte32::from_slice(&dao_tx_hash).unwrap())
        .index(0u32)
        .build();
    let vote_type = Script::new_builder()
        .code_hash(Byte32::from_slice(&vote_code_hash).unwrap())
        .hash_type(ScriptHashType::Type)
        .args(Bytes::from(proposal_id.to_vec()).pack())
        .build();
    let vote_data = VoteData {
        direction: u8::from(!voter.is_multiple_of(3)),
        amount: 500 * CKB,
        dao_dep_indices: vec![0],
    }
    .encode()
    .unwrap();
    RawTransaction::new_builder()
        .version(voter)
        .cell_deps([CellDep::new_builder().out_point(dao_out_point).build()].pack())
        .outputs(
            [CellOutput::new_builder()
                .capacity(500 * CKB)
                .lock(voter_lock)
                .type_(Some(vote_type).pack())
                .build()]
            .pack(),
        )
        .outputs_data([Bytes::from(vote_data)].pack())
        .build()
}

#[test]
fn vote_contract_validates_configured_dao_type_and_amount() {
    let mut context = Context::default();
    let vote_code = context.deploy_cell_by_name("vote-type-script");
    let always_success = context.deploy_cell(ALWAYS_SUCCESS.clone());
    let owner_lock = context
        .build_script(&always_success, Bytes::from(vec![1]))
        .unwrap();
    let proposal_type = context
        .build_script(&always_success, Bytes::from(vec![2]))
        .unwrap();
    let proposal_id: [u8; 32] = proposal_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    let dao_type = context.build_script(&always_success, Bytes::new()).unwrap();
    let config_type = context
        .build_script(&always_success, Bytes::from(vec![3]))
        .unwrap();
    let vote_type = context
        .build_script(&vote_code, Bytes::from(proposal_id.to_vec()))
        .unwrap();
    let mut proposal = proposal(
        proposal_id,
        vote_type.code_hash().as_slice().try_into().unwrap(),
    );
    proposal.phase = ProposalPhase::Open;
    proposal.proposal_config_type_hash = config_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();

    let proposal_config = ProposalConfig {
        approval_bps: 6_000,
        minimum_total_votes: 100 * CKB as u128,
        maximum_proposal_amount: 1_000 * CKB,
        minimum_challenge_period: 5,
        treasury_lock_hash: [7; 32],
        dao_code_hash: dao_type.code_hash().as_slice().try_into().unwrap(),
        dao_hash_type: dao_type.hash_type().as_slice()[0],
        proposal_code_hash: proposal_type.code_hash().as_slice().try_into().unwrap(),
        proposal_hash_type: proposal_type.hash_type().as_slice()[0],
        vote_code_hash: vote_type.code_hash().as_slice().try_into().unwrap(),
        vote_hash_type: vote_type.hash_type().as_slice()[0],
        tally_code_hash: [3; 32],
        tally_hash_type: 1,
        candidate_lock_hash: [14; 32],
        policy_type_hash: [4; 32],
    };
    let config_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(owner_lock.clone())
            .type_(Some(config_type).pack())
            .build(),
        Bytes::from(proposal_config.encode()),
    );

    let proposal_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(owner_lock.clone())
            .type_(Some(proposal_type.clone()).pack())
            .build(),
        Bytes::from(proposal.encode()),
    );
    let dao_capacity = 500 * CKB;
    let dao_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(dao_capacity)
            .lock(owner_lock.clone())
            .type_(Some(dao_type).pack())
            .build(),
        Bytes::from(vec![0; 8]),
    );
    let owner_input = context.create_cell(
        CellOutput::new_builder()
            .capacity(600 * CKB)
            .lock(owner_lock.clone())
            .build(),
        Bytes::new(),
    );
    let vote_data = VoteData {
        direction: 1,
        amount: dao_capacity,
        dao_dep_indices: vec![2],
    }
    .encode()
    .unwrap();
    let tx = TransactionBuilder::default()
        .cell_dep(
            CellDep::new_builder()
                .out_point(proposal_cell.clone())
                .build(),
        )
        .cell_dep(
            CellDep::new_builder()
                .out_point(config_cell.clone())
                .build(),
        )
        .cell_dep(CellDep::new_builder().out_point(dao_cell.clone()).build())
        .input(
            CellInput::new_builder()
                .previous_output(owner_input.clone())
                .build(),
        )
        .output(
            CellOutput::new_builder()
                .capacity(500 * CKB)
                .lock(owner_lock.clone())
                .type_(Some(vote_type.clone()).pack())
                .build(),
        )
        .output_data(Bytes::from(vote_data).pack())
        .build();
    let tx = context.complete_tx(tx);
    context.verify_tx(&tx, 20_000_000).unwrap();

    let wrong_amount_vote_data = VoteData {
        direction: 1,
        amount: dao_capacity - CKB,
        dao_dep_indices: vec![2],
    }
    .encode()
    .unwrap();
    let wrong_amount = TransactionBuilder::default()
        .cell_dep(
            CellDep::new_builder()
                .out_point(proposal_cell.clone())
                .build(),
        )
        .cell_dep(
            CellDep::new_builder()
                .out_point(config_cell.clone())
                .build(),
        )
        .cell_dep(CellDep::new_builder().out_point(dao_cell.clone()).build())
        .input(
            CellInput::new_builder()
                .previous_output(owner_input.clone())
                .build(),
        )
        .output(
            CellOutput::new_builder()
                .capacity(500 * CKB)
                .lock(owner_lock.clone())
                .type_(Some(vote_type.clone()).pack())
                .build(),
        )
        .output_data(Bytes::from(wrong_amount_vote_data).pack())
        .build();
    let wrong_amount = context.complete_tx(wrong_amount);
    assert!(context.verify_tx(&wrong_amount, 20_000_000).is_err());

    let forged_dao_type = Script::new_builder()
        .code_hash(Byte32::from_slice(&[0x99; 32]).unwrap())
        .hash_type(ScriptHashType::Type)
        .build();
    let forged_dao_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(dao_capacity)
            .lock(owner_lock.clone())
            .type_(Some(forged_dao_type).pack())
            .build(),
        Bytes::from(vec![0; 8]),
    );
    let forged_vote_data = VoteData {
        direction: 1,
        amount: dao_capacity,
        dao_dep_indices: vec![2],
    }
    .encode()
    .unwrap();
    let forged = TransactionBuilder::default()
        .cell_dep(
            CellDep::new_builder()
                .out_point(proposal_cell.clone())
                .build(),
        )
        .cell_dep(
            CellDep::new_builder()
                .out_point(config_cell.clone())
                .build(),
        )
        .cell_dep(CellDep::new_builder().out_point(forged_dao_cell).build())
        .input(
            CellInput::new_builder()
                .previous_output(owner_input)
                .build(),
        )
        .output(
            CellOutput::new_builder()
                .capacity(500 * CKB)
                .lock(owner_lock.clone())
                .type_(Some(vote_type.clone()).pack())
                .build(),
        )
        .output_data(Bytes::from(forged_vote_data).pack())
        .build();
    let forged = context.complete_tx(forged);
    assert!(context.verify_tx(&forged, 20_000_000).is_err());

    let invalid_vote_data = VoteData {
        direction: 1,
        amount: dao_capacity,
        dao_dep_indices: vec![2],
    }
    .encode()
    .unwrap();
    let invalid = TransactionBuilder::default()
        .cell_dep(CellDep::new_builder().out_point(proposal_cell).build())
        .cell_dep(CellDep::new_builder().out_point(config_cell).build())
        .cell_dep(CellDep::new_builder().out_point(dao_cell.clone()).build())
        .input(CellInput::new_builder().previous_output(dao_cell).build())
        .output(
            CellOutput::new_builder()
                .capacity(500 * CKB)
                .lock(owner_lock)
                .type_(Some(vote_type).pack())
                .build(),
        )
        .output_data(Bytes::from(invalid_vote_data).pack())
        .build();
    let invalid = context.complete_tx(invalid);
    assert!(context.verify_tx(&invalid, 20_000_000).is_err());
}

#[test]
fn policy_contract_rejects_proposal_bound_to_different_config() {
    let mut context = Context::default();
    let policy_code = context.deploy_cell_by_name("policy-type-script");
    let always_success = context.deploy_cell(ALWAYS_SUCCESS.clone());
    let owner_lock = context
        .build_script(&always_success, Bytes::from(vec![1]))
        .unwrap();
    let config_type = context
        .build_script(&always_success, Bytes::from(vec![2]))
        .unwrap();
    let proposal_type = context
        .build_script(&always_success, Bytes::from(vec![3]))
        .unwrap();
    let tally_type = context
        .build_script_with_hash_type(&always_success, ScriptHashType::Data1, Bytes::from(vec![4]))
        .unwrap();
    let policy_type = context
        .build_script(&policy_code, config_type.calc_script_hash().as_bytes())
        .unwrap();
    let config = ProposalConfig {
        approval_bps: 6_000,
        minimum_total_votes: 1,
        maximum_proposal_amount: 1_000 * CKB,
        minimum_challenge_period: 5,
        treasury_lock_hash: [7; 32],
        dao_code_hash: [8; 32],
        dao_hash_type: 1,
        proposal_code_hash: proposal_type.code_hash().as_slice().try_into().unwrap(),
        proposal_hash_type: proposal_type.hash_type().as_slice()[0],
        vote_code_hash: [5; 32],
        vote_hash_type: 1,
        tally_code_hash: tally_type.code_hash().as_slice().try_into().unwrap(),
        tally_hash_type: tally_type.hash_type().as_slice()[0],
        candidate_lock_hash: [14; 32],
        policy_type_hash: policy_type
            .calc_script_hash()
            .as_slice()
            .try_into()
            .unwrap(),
    };
    let config_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(owner_lock.clone())
            .type_(Some(config_type.clone()).pack())
            .build(),
        Bytes::from(config.encode()),
    );
    let proposal_id = proposal_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    let mut proposal = proposal(proposal_id, [5; 32]);
    proposal.proposal_config_type_hash = config_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();

    let candidate = TallyState {
        phase: TallyPhase::Candidate,
        proposal_id,
        operator_lock_hash: [9; 32],
        sequence: 1,
        next_block: proposal.end_block + 1,
        next_tx_index: 0,
        votes_root: [10; 32],
        dao_root: [11; 32],
        events_root: [12; 32],
        yes: 2_000 * CKB as u128,
        no: 0,
        processed_events: 1,
        candidate_since: proposal.end_block + 1,
    };
    let candidate_data = candidate.encode();
    let candidate_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(2_000 * CKB)
            .lock(owner_lock.clone())
            .type_(Some(tally_type).pack())
            .build(),
        Bytes::from(candidate_data.clone()),
    );
    let result = ResultData {
        passed: true,
        proposal_id,
        requested_amount: proposal.requested_amount,
        receiver_lock_hash: proposal.receiver_lock_hash,
        yes: candidate.yes,
        no: candidate.no,
        final_state_hash: blake2b_256(&candidate_data),
        proposal_config_data_hash: blake2b_256(&config.encode()),
    };
    let valid_proposal_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(owner_lock.clone())
            .type_(Some(proposal_type.clone()).pack())
            .build(),
        Bytes::from(proposal.encode()),
    );
    let mut forged_proposal = proposal;
    forged_proposal.proposal_config_type_hash = [0x99; 32];
    let forged_proposal_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(owner_lock.clone())
            .type_(Some(proposal_type).pack())
            .build(),
        Bytes::from(forged_proposal.encode()),
    );
    let build = |proposal_cell: OutPoint| {
        TransactionBuilder::default()
            .cell_dep(
                CellDep::new_builder()
                    .out_point(config_cell.clone())
                    .build(),
            )
            .input(
                CellInput::new_builder()
                    .previous_output(proposal_cell)
                    .build(),
            )
            .input(
                CellInput::new_builder()
                    .previous_output(candidate_cell.clone())
                    .build(),
            )
            .output(
                CellOutput::new_builder()
                    .capacity(1_000 * CKB)
                    .lock(owner_lock.clone())
                    .type_(Some(policy_type.clone()).pack())
                    .build(),
            )
            .output_data(Bytes::from(result.encode()).pack())
            .build()
    };
    let valid = context.complete_tx(build(valid_proposal_cell));
    context.verify_tx(&valid, 20_000_000).unwrap();

    let forged = context.complete_tx(build(forged_proposal_cell));
    assert!(context.verify_tx(&forged, 20_000_000).is_err());
}

#[test]
fn tally_contract_accepts_builder_generated_final_batch() {
    let mut context = Context::default();
    let tally_code = context.deploy_cell_by_name("tally-type-script");
    let always_success = context.deploy_cell(ALWAYS_SUCCESS.clone());
    let session_lock = context
        .build_script(&always_success, Bytes::from(vec![1]))
        .unwrap();
    let candidate_lock = context
        .build_script(&always_success, Bytes::from(vec![0xca]))
        .unwrap();
    let proposal_type = context
        .build_script(&always_success, Bytes::from(vec![2]))
        .unwrap();
    let proposal_id: [u8; 32] = proposal_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    let vote_code_hash = [0x22; 32];
    let tally_script = context
        .build_script(&tally_code, Bytes::from(vec![0x77; 32]))
        .unwrap();
    let config = tally_proposal_config(
        &proposal_type,
        vote_code_hash,
        &tally_script,
        &candidate_lock,
    );
    let (config_cell, config_hash) =
        create_proposal_config_dep(&mut context, &always_success, &session_lock, config);
    let mut proposal = proposal(proposal_id, vote_code_hash);
    proposal.proposal_config_type_hash = config_hash;
    let proposal_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(session_lock.clone())
            .type_(Some(proposal_type).pack())
            .build(),
        Bytes::from(proposal.encode()),
    );

    let historical_raw = historical_vote_raw(proposal_id, vote_code_hash, session_lock.clone(), 0);
    let historical_raw_bytes = historical_raw.as_slice().to_vec();
    let witnesses_root = [0x66; 32];
    let proven =
        prove_block_transactions(10, 0, &[historical_raw_bytes], witnesses_root, &[0]).unwrap();
    let raw_tx_hash: [u8; 32] = historical_raw.calc_tx_hash().as_slice().try_into().unwrap();
    let historical_header = HeaderBuilder::default()
        .number(10u64)
        .epoch(EpochNumberWithFraction::new(0, 10, 100))
        .transactions_root(Byte32::from_slice(&hash_pair(&raw_tx_hash, &witnesses_root)).unwrap())
        .build();
    let anchor_header = HeaderBuilder::default()
        .number(11u64)
        .epoch(EpochNumberWithFraction::new(0, 11, 100))
        .build();
    context.insert_header(historical_header.clone());
    context.insert_header(anchor_header.clone());

    let operator_lock_hash = session_lock
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    let mut builder = TallyBuilder::new(
        proposal_id,
        operator_lock_hash,
        proposal.start_block,
        config,
    );
    let input_state = builder.state().clone();
    let (output_state, batch) = builder
        .build_batch(
            &proposal,
            vec![proven],
            proposal.end_block + 1,
            0,
            proposal.end_block,
        )
        .unwrap();
    assert_eq!(output_state.phase, TallyPhase::Candidate);
    assert_eq!(output_state.yes, 500u128 * CKB as u128);

    let tally_input = context.create_cell(
        CellOutput::new_builder()
            .capacity(2_000 * CKB)
            .lock(session_lock.clone())
            .type_(Some(tally_script.clone()).pack())
            .build(),
        Bytes::from(input_state.encode()),
    );
    let witness = WitnessArgs::new_builder()
        .input_type(Some(Bytes::from(TallyWitness::Advance(batch).encode().unwrap())).pack())
        .build();
    let build = |output_lock: Script| {
        TransactionBuilder::default()
            .cell_dep(
                CellDep::new_builder()
                    .out_point(proposal_cell.clone())
                    .build(),
            )
            .cell_dep(
                CellDep::new_builder()
                    .out_point(config_cell.clone())
                    .build(),
            )
            .header_dep(historical_header.hash())
            .header_dep(anchor_header.hash())
            .input(
                CellInput::new_builder()
                    .previous_output(tally_input.clone())
                    .build(),
            )
            .output(
                CellOutput::new_builder()
                    .capacity(2_000 * CKB)
                    .lock(output_lock)
                    .type_(Some(tally_script.clone()).pack())
                    .build(),
            )
            .output_data(Bytes::from(output_state.encode()).pack())
            .witness(witness.as_bytes().pack())
            .build()
    };
    let wrong_lock = context.complete_tx(build(session_lock));
    assert!(context.verify_tx(&wrong_lock, 100_000_000).is_err());

    let tx = context.complete_tx(build(candidate_lock));
    let cycles = context.verify_tx(&tx, 100_000_000).unwrap();
    assert!(cycles > 0);
}

fn build_tally_batch_tx(
    vote_count: usize,
    include_fillers: bool,
    mutate: impl FnOnce(&mut TallyState, &mut BatchWitness),
) -> (Context, TransactionView, usize, usize) {
    let mut context = Context::default();
    let tally_code = context.deploy_cell_by_name("tally-type-script");
    let always_success = context.deploy_cell(ALWAYS_SUCCESS.clone());
    let session_lock = context
        .build_script(&always_success, Bytes::from(vec![1]))
        .unwrap();
    let proposal_type = context
        .build_script(&always_success, Bytes::from(vec![2]))
        .unwrap();
    let proposal_id = proposal_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    let vote_code_hash = [0x22; 32];
    let tally_script = context
        .build_script(&tally_code, Bytes::from(vec![0x7a; 32]))
        .unwrap();
    let config =
        tally_proposal_config(&proposal_type, vote_code_hash, &tally_script, &session_lock);
    let (config_cell, config_hash) =
        create_proposal_config_dep(&mut context, &always_success, &session_lock, config);
    let mut proposal = proposal(proposal_id, vote_code_hash);
    proposal.proposal_config_type_hash = config_hash;
    proposal.max_events_per_batch = 1_000;
    proposal.max_state_keys_per_batch = 4096;
    proposal.max_batch_witness_bytes = 2_000_000;
    let proposal_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(session_lock.clone())
            .type_(Some(proposal_type).pack())
            .build(),
        Bytes::from(proposal.encode()),
    );

    let mut raw_transactions = Vec::new();
    let mut event_indices = Vec::new();
    for voter in 0..vote_count {
        let voter_lock = context
            .build_script(
                &always_success,
                Bytes::from((voter as u32).to_le_bytes().to_vec()),
            )
            .unwrap();
        event_indices.push(raw_transactions.len() as u32);
        raw_transactions.push(
            historical_unique_vote_raw(
                proposal_id,
                vote_code_hash,
                voter_lock.clone(),
                voter as u32,
            )
            .as_slice()
            .to_vec(),
        );
        if include_fillers {
            let filler = RawTransaction::new_builder()
                .outputs([CellOutput::new_builder().lock(voter_lock).build()].pack())
                .outputs_data([Bytes::new()].pack())
                .build();
            raw_transactions.push(filler.as_slice().to_vec());
        }
    }
    let raw_hashes = raw_transactions
        .iter()
        .map(|raw| {
            RawTransaction::from_slice(raw)
                .unwrap()
                .calc_tx_hash()
                .as_slice()
                .try_into()
                .unwrap()
        })
        .collect::<Vec<[u8; 32]>>();
    let raw_root = CBMT::<[u8; 32], MergeHash>::build_merkle_root(&raw_hashes);
    let witnesses_root = [0x66; 32];
    let historical_header = HeaderBuilder::default()
        .number(proposal.start_block)
        .epoch(EpochNumberWithFraction::new(0, 10, 100))
        .transactions_root(
            Byte32::from_slice(&transactions_root(raw_root, witnesses_root)).unwrap(),
        )
        .build();
    let anchor_header = HeaderBuilder::default()
        .number(proposal.end_block)
        .epoch(EpochNumberWithFraction::new(0, 11, 100))
        .build();
    context.insert_header(historical_header.clone());
    context.insert_header(anchor_header.clone());

    let blocks = vec![
        prove_block_transactions(
            proposal.start_block,
            0,
            &raw_transactions,
            witnesses_root,
            &event_indices,
        )
        .unwrap(),
    ];
    let operator_lock_hash = session_lock
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    let mut builder = TallyBuilder::new(
        proposal_id,
        operator_lock_hash,
        proposal.start_block,
        config,
    );
    let input_state = builder.state().clone();
    let (mut output_state, mut batch) = builder
        .build_batch(
            &proposal,
            blocks,
            proposal.end_block + 1,
            0,
            proposal.end_block,
        )
        .unwrap();
    mutate(&mut output_state, &mut batch);

    let tally_input = context.create_cell(
        CellOutput::new_builder()
            .capacity(2_000 * CKB)
            .lock(session_lock.clone())
            .type_(Some(tally_script.clone()).pack())
            .build(),
        Bytes::from(input_state.encode()),
    );
    let batch_witness = TallyWitness::Advance(batch).encode().unwrap();
    let batch_witness_bytes = batch_witness.len();
    let witness = WitnessArgs::new_builder()
        .input_type(Some(Bytes::from(batch_witness)).pack())
        .build();
    let tx = TransactionBuilder::default()
        .cell_dep(CellDep::new_builder().out_point(proposal_cell).build())
        .cell_dep(CellDep::new_builder().out_point(config_cell).build())
        .header_dep(historical_header.hash())
        .header_dep(anchor_header.hash())
        .input(
            CellInput::new_builder()
                .previous_output(tally_input)
                .build(),
        )
        .output(
            CellOutput::new_builder()
                .capacity(2_000 * CKB)
                .lock(session_lock)
                .type_(Some(tally_script).pack())
                .build(),
        )
        .output_data(Bytes::from(output_state.encode()).pack())
        .witness(witness.as_bytes().pack())
        .build();
    let tx = context.complete_tx(tx);
    let transaction_bytes = tx.data().serialized_size_in_block();
    (context, tx, batch_witness_bytes, transaction_bytes)
}

fn benchmark_tally_batch(vote_count: usize) -> (u64, usize, usize) {
    let (context, tx, batch_witness_bytes, transaction_bytes) =
        build_tally_batch_tx(vote_count, false, |_, _| {});
    let cycles = context.verify_tx(&tx, 3_500_000_000).unwrap();
    (cycles, batch_witness_bytes, transaction_bytes)
}

#[test]
#[ignore = "cycle benchmark"]
fn benchmark_tally_batch_cycles() {
    for vote_count in [1usize, 10, 20, 21, 22, 23, 50, 100, 200, 500] {
        let (cycles, witness_bytes, transaction_bytes) = benchmark_tally_batch(vote_count);
        println!(
            "votes={vote_count} cycles={cycles} cycles_per_vote={} witness_bytes={witness_bytes} transaction_bytes={transaction_bytes}",
            cycles / vote_count as u64
        );
    }
}

#[test]
fn tally_contract_rejects_malformed_block_multiproofs() {
    let (context, tx, _, _) = build_tally_batch_tx(2, false, |_, batch| {
        batch.blocks[0].transactions[1].tx_index = batch.blocks[0].transactions[0].tx_index;
    });
    assert!(context.verify_tx(&tx, 100_000_000).is_err());

    let (context, tx, _, _) = build_tally_batch_tx(2, false, |_, batch| {
        batch.blocks[0].transactions.swap(0, 1);
    });
    assert!(context.verify_tx(&tx, 100_000_000).is_err());

    let (context, tx, _, _) = build_tally_batch_tx(2, false, |_, batch| {
        batch.blocks[0].lemmas.push([0x55; 32]);
    });
    assert!(context.verify_tx(&tx, 100_000_000).is_err());

    let (context, tx, _, _) = build_tally_batch_tx(2, true, |_, batch| {
        assert!(!batch.blocks[0].lemmas.is_empty());
        batch.blocks[0].lemmas.pop();
    });
    assert!(context.verify_tx(&tx, 100_000_000).is_err());

    let (context, tx, _, _) = build_tally_batch_tx(2, false, |_, batch| {
        batch.blocks[0].block_number += 1;
    });
    assert!(context.verify_tx(&tx, 100_000_000).is_err());

    let (context, tx, _, _) = build_tally_batch_tx(2, false, |_, batch| {
        let raw = RawTransaction::from_slice(&batch.blocks[0].transactions[0].raw_transaction)
            .unwrap()
            .as_builder()
            .version(42u32)
            .build();
        batch.blocks[0].transactions[0].raw_transaction = raw.as_slice().to_vec();
    });
    assert!(context.verify_tx(&tx, 100_000_000).is_err());
}

#[test]
fn tally_contract_rejects_incorrect_output_amount() {
    let (context, tx, _, _) = build_tally_batch_tx(1, false, |output, _| {
        output.no -= CKB as u128;
    });
    assert!(context.verify_tx(&tx, 100_000_000).is_err());
}

#[test]
fn tally_contract_rejects_tampered_proven_vote_amount() {
    let (context, tx, _, _) = build_tally_batch_tx(1, false, |_, batch| {
        let proven_vote = &mut batch.blocks[0].transactions[0];
        let raw = RawTransaction::from_slice(&proven_vote.raw_transaction).unwrap();
        let output_data = raw.outputs_data().get(0).unwrap().raw_data();
        let mut vote = VoteData::decode(&output_data).unwrap();
        vote.amount -= CKB;
        let tampered = raw
            .as_builder()
            .outputs_data([Bytes::from(vote.encode().unwrap())].pack())
            .build();
        proven_vote.raw_transaction = tampered.as_slice().to_vec();
    });
    assert!(context.verify_tx(&tx, 100_000_000).is_err());
}

#[test]
fn tally_contract_accepts_partial_block_multiproof() {
    let (context, tx, _, _) = build_tally_batch_tx(2, true, |_, batch| {
        assert!(!batch.blocks[0].lemmas.is_empty());
        assert_eq!(batch.blocks[0].transactions.len(), 2);
        assert_eq!(batch.blocks[0].tx_count, 4);
    });
    context.verify_tx(&tx, 100_000_000).unwrap();
}

#[test]
fn tally_contract_accepts_empty_final_batch() {
    let mut context = Context::default();
    let tally_code = context.deploy_cell_by_name("tally-type-script");
    let always_success = context.deploy_cell(ALWAYS_SUCCESS.clone());
    let session_lock = context
        .build_script(&always_success, Bytes::from(vec![1]))
        .unwrap();
    let proposal_type = context
        .build_script(&always_success, Bytes::from(vec![2]))
        .unwrap();
    let proposal_id = proposal_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    let vote_code_hash = [0x22; 32];
    let tally_script = context
        .build_script(&tally_code, Bytes::from(vec![0x78; 32]))
        .unwrap();
    let config =
        tally_proposal_config(&proposal_type, vote_code_hash, &tally_script, &session_lock);
    let (config_cell, config_hash) =
        create_proposal_config_dep(&mut context, &always_success, &session_lock, config);
    let mut proposal = proposal(proposal_id, vote_code_hash);
    proposal.proposal_config_type_hash = config_hash;
    let proposal_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(session_lock.clone())
            .type_(Some(proposal_type).pack())
            .build(),
        Bytes::from(proposal.encode()),
    );
    let anchor_header = HeaderBuilder::default()
        .number(proposal.end_block)
        .epoch(EpochNumberWithFraction::new(0, 11, 100))
        .build();
    context.insert_header(anchor_header.clone());

    let operator_lock_hash = session_lock
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    let mut builder = TallyBuilder::new(
        proposal_id,
        operator_lock_hash,
        proposal.start_block,
        config,
    );
    let input_state = builder.state().clone();
    let (output_state, batch) = builder
        .build_batch(
            &proposal,
            Vec::new(),
            proposal.end_block + 1,
            0,
            proposal.end_block,
        )
        .unwrap();

    let tally_input = context.create_cell(
        CellOutput::new_builder()
            .capacity(2_000 * CKB)
            .lock(session_lock.clone())
            .type_(Some(tally_script.clone()).pack())
            .build(),
        Bytes::from(input_state.encode()),
    );
    let witness = WitnessArgs::new_builder()
        .input_type(Some(Bytes::from(TallyWitness::Advance(batch).encode().unwrap())).pack())
        .build();
    let tx = TransactionBuilder::default()
        .cell_dep(CellDep::new_builder().out_point(proposal_cell).build())
        .cell_dep(CellDep::new_builder().out_point(config_cell).build())
        .header_dep(anchor_header.hash())
        .input(
            CellInput::new_builder()
                .previous_output(tally_input)
                .build(),
        )
        .output(
            CellOutput::new_builder()
                .capacity(2_000 * CKB)
                .lock(session_lock)
                .type_(Some(tally_script).pack())
                .build(),
        )
        .output_data(Bytes::from(output_state.encode()).pack())
        .witness(witness.as_bytes().pack())
        .build();
    let tx = context.complete_tx(tx);
    context.verify_tx(&tx, 100_000_000).unwrap();
}

#[test]
fn omitted_vote_challenge_slashes_candidate_bond() {
    let mut context = Context::default();
    let tally_code = context.deploy_cell_by_name("tally-type-script");
    let always_success = context.deploy_cell(ALWAYS_SUCCESS.clone());
    let session_lock = context
        .build_script(&always_success, Bytes::from(vec![1]))
        .unwrap();
    let challenger_lock = context
        .build_script(&always_success, Bytes::from(vec![9]))
        .unwrap();
    let proposal_type = context
        .build_script(&always_success, Bytes::from(vec![2]))
        .unwrap();
    let proposal_id = proposal_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    let vote_code_hash = [0x22; 32];
    let tally_script = context
        .build_script(&tally_code, Bytes::from(vec![0x77; 32]))
        .unwrap();
    let config =
        tally_proposal_config(&proposal_type, vote_code_hash, &tally_script, &session_lock);
    let (config_cell, config_hash) =
        create_proposal_config_dep(&mut context, &always_success, &session_lock, config);
    let mut proposal = proposal(proposal_id, vote_code_hash);
    proposal.proposal_config_type_hash = config_hash;
    let proposal_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(session_lock.clone())
            .type_(Some(proposal_type).pack())
            .build(),
        Bytes::from(proposal.encode()),
    );

    let included_raw = historical_vote_raw(proposal_id, vote_code_hash, session_lock.clone(), 0);
    let included_witnesses_root = [0x66; 32];
    let included = prove_block_transactions(
        10,
        0,
        &[included_raw.as_slice().to_vec()],
        included_witnesses_root,
        &[0],
    )
    .unwrap();
    let mut builder = TallyBuilder::new(
        proposal_id,
        session_lock
            .calc_script_hash()
            .as_slice()
            .try_into()
            .unwrap(),
        proposal.start_block,
        config,
    );
    let (candidate, _) = builder
        .build_batch(
            &proposal,
            vec![included],
            proposal.end_block + 1,
            0,
            proposal.end_block,
        )
        .unwrap();

    let omitted_raw = historical_vote_raw(proposal_id, vote_code_hash, session_lock.clone(), 1);
    let omitted_witnesses_root = [0x88; 32];
    let omitted = prove_transaction(
        11,
        0,
        &[omitted_raw.as_slice().to_vec()],
        omitted_witnesses_root,
        0,
    )
    .unwrap();
    let omitted_hash: [u8; 32] = omitted_raw.calc_tx_hash().as_slice().try_into().unwrap();
    let omitted_header = HeaderBuilder::default()
        .number(11u64)
        .epoch(EpochNumberWithFraction::new(0, 11, 100))
        .transactions_root(
            Byte32::from_slice(&hash_pair(&omitted_hash, &omitted_witnesses_root)).unwrap(),
        )
        .build();
    context.insert_header(omitted_header.clone());
    let challenge = builder
        .build_omitted_vote_challenge(
            challenger_lock
                .calc_script_hash()
                .as_slice()
                .try_into()
                .unwrap(),
            omitted,
        )
        .unwrap();

    let bond = 2_000 * CKB;
    let candidate_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(bond)
            .lock(session_lock)
            .type_(Some(tally_script).pack())
            .build(),
        Bytes::from(candidate.encode()),
    );
    let witness = WitnessArgs::new_builder()
        .input_type(Some(Bytes::from(challenge.encode().unwrap())).pack())
        .build();
    let tx = TransactionBuilder::default()
        .cell_dep(CellDep::new_builder().out_point(proposal_cell).build())
        .cell_dep(CellDep::new_builder().out_point(config_cell).build())
        .header_dep(omitted_header.hash())
        .input(
            CellInput::new_builder()
                .previous_output(candidate_cell)
                .build(),
        )
        .output(
            CellOutput::new_builder()
                .capacity(bond)
                .lock(challenger_lock)
                .build(),
        )
        .output_data(Bytes::new().pack())
        .witness(witness.as_bytes().pack())
        .build();
    let tx = context.complete_tx(tx);
    context.verify_tx(&tx, 100_000_000).unwrap();
}

#[test]
fn omitted_live_dao_spend_challenge_slashes_candidate_bond() {
    let mut context = Context::default();
    let tally_code = context.deploy_cell_by_name("tally-type-script");
    let always_success = context.deploy_cell(ALWAYS_SUCCESS.clone());
    let session_lock = context
        .build_script(&always_success, Bytes::from(vec![1]))
        .unwrap();
    let challenger_lock = context
        .build_script(&always_success, Bytes::from(vec![9]))
        .unwrap();
    let proposal_type = context
        .build_script(&always_success, Bytes::from(vec![2]))
        .unwrap();
    let proposal_id = proposal_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    let vote_code_hash = [0x22; 32];
    let tally_script = context
        .build_script(&tally_code, Bytes::from(vec![0x79; 32]))
        .unwrap();
    let config =
        tally_proposal_config(&proposal_type, vote_code_hash, &tally_script, &session_lock);
    let (config_cell, config_hash) =
        create_proposal_config_dep(&mut context, &always_success, &session_lock, config);
    let mut proposal = proposal(proposal_id, vote_code_hash);
    proposal.proposal_config_type_hash = config_hash;
    let proposal_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(session_lock.clone())
            .type_(Some(proposal_type).pack())
            .build(),
        Bytes::from(proposal.encode()),
    );

    let vote_raw = historical_vote_raw(proposal_id, vote_code_hash, session_lock.clone(), 0);
    let vote =
        prove_block_transactions(10, 0, &[vote_raw.as_slice().to_vec()], [0x66; 32], &[0]).unwrap();
    let mut builder = TallyBuilder::new(
        proposal_id,
        session_lock
            .calc_script_hash()
            .as_slice()
            .try_into()
            .unwrap(),
        proposal.start_block,
        config,
    );
    let (candidate, _) = builder
        .build_batch(
            &proposal,
            vec![vote],
            proposal.end_block + 1,
            0,
            proposal.end_block,
        )
        .unwrap();

    let spend_raw = historical_dao_spend_raw();
    let witnesses_root = [0x88; 32];
    let omitted_spend =
        prove_transaction(11, 0, &[spend_raw.as_slice().to_vec()], witnesses_root, 0).unwrap();
    let spend_hash: [u8; 32] = spend_raw.calc_tx_hash().as_slice().try_into().unwrap();
    let spend_header = HeaderBuilder::default()
        .number(11u64)
        .epoch(EpochNumberWithFraction::new(0, 11, 100))
        .transactions_root(Byte32::from_slice(&hash_pair(&spend_hash, &witnesses_root)).unwrap())
        .build();
    context.insert_header(spend_header.clone());
    let challenge = builder
        .build_omitted_spend_challenge(
            challenger_lock
                .calc_script_hash()
                .as_slice()
                .try_into()
                .unwrap(),
            omitted_spend,
            treasury_common::OutPoint {
                tx_hash: [0x44; 32],
                index: 1,
            },
        )
        .unwrap();

    let bond = 2_000 * CKB;
    let candidate_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(bond)
            .lock(session_lock)
            .type_(Some(tally_script).pack())
            .build(),
        Bytes::from(candidate.encode()),
    );
    let witness = WitnessArgs::new_builder()
        .input_type(Some(Bytes::from(challenge.encode().unwrap())).pack())
        .build();
    let tx = TransactionBuilder::default()
        .cell_dep(CellDep::new_builder().out_point(proposal_cell).build())
        .cell_dep(CellDep::new_builder().out_point(config_cell).build())
        .header_dep(spend_header.hash())
        .input(
            CellInput::new_builder()
                .previous_output(candidate_cell)
                .build(),
        )
        .output(
            CellOutput::new_builder()
                .capacity(bond)
                .lock(challenger_lock)
                .build(),
        )
        .output_data(Bytes::new().pack())
        .witness(witness.as_bytes().pack())
        .build();
    let tx = context.complete_tx(tx);
    context.verify_tx(&tx, 100_000_000).unwrap();
}

#[test]
fn tally_finalize_requires_candidate_relative_since() {
    let mut context = Context::default();
    let tally_code = context.deploy_cell_by_name("tally-type-script");
    let always_success = context.deploy_cell(ALWAYS_SUCCESS.clone());
    let operator_lock = context
        .build_script(&always_success, Bytes::from(vec![1]))
        .unwrap();
    let candidate_lock = context
        .build_script(&always_success, Bytes::from(vec![2]))
        .unwrap();
    let proposal_type = context
        .build_script(&always_success, Bytes::from(vec![3]))
        .unwrap();
    let proposal_id = proposal_type
        .calc_script_hash()
        .as_slice()
        .try_into()
        .unwrap();
    let tally_type = context
        .build_script(&tally_code, Bytes::from(vec![0x7b; 32]))
        .unwrap();
    let config = tally_proposal_config(&proposal_type, [0x22; 32], &tally_type, &candidate_lock);
    let (config_cell, config_hash) =
        create_proposal_config_dep(&mut context, &always_success, &operator_lock, config);
    let mut proposal = proposal(proposal_id, [0x22; 32]);
    proposal.proposal_config_type_hash = config_hash;
    let proposal_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(operator_lock.clone())
            .type_(Some(proposal_type).pack())
            .build(),
        Bytes::from(proposal.encode()),
    );
    let candidate = TallyState {
        phase: TallyPhase::Candidate,
        proposal_id,
        operator_lock_hash: operator_lock
            .calc_script_hash()
            .as_slice()
            .try_into()
            .unwrap(),
        sequence: 1,
        next_block: proposal.end_block + 1,
        next_tx_index: 0,
        votes_root: [0; 32],
        dao_root: [0; 32],
        events_root: [0; 32],
        yes: 0,
        no: 0,
        processed_events: 0,
        candidate_since: proposal.end_block,
    };
    let bond = 2_000 * CKB;
    let candidate_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(bond)
            .lock(candidate_lock)
            .type_(Some(tally_type).pack())
            .build(),
        Bytes::from(candidate.encode()),
    );
    let operator_auth = context.create_cell(
        CellOutput::new_builder()
            .capacity(100 * CKB)
            .lock(operator_lock.clone())
            .build(),
        Bytes::new(),
    );
    let finalize_witness = WitnessArgs::new_builder()
        .input_type(Some(Bytes::from(TallyWitness::Finalize.encode().unwrap())).pack())
        .build();
    let build = |relative_blocks: u64| {
        TransactionBuilder::default()
            .cell_dep(
                CellDep::new_builder()
                    .out_point(config_cell.clone())
                    .build(),
            )
            .input(
                CellInput::new_builder()
                    .since(0x8000_0000_0000_0000 | relative_blocks)
                    .previous_output(candidate_cell.clone())
                    .build(),
            )
            .input(
                CellInput::new_builder()
                    .previous_output(proposal_cell.clone())
                    .build(),
            )
            .input(
                CellInput::new_builder()
                    .previous_output(operator_auth.clone())
                    .build(),
            )
            .output(
                CellOutput::new_builder()
                    .capacity(bond)
                    .lock(operator_lock.clone())
                    .build(),
            )
            .output_data(Bytes::new().pack())
            .witness(finalize_witness.as_bytes().pack())
            .build()
    };

    let backdated = context.complete_tx(build(0));
    assert!(context.verify_tx(&backdated, 100_000_000).is_err());
    let too_early = context.complete_tx(build(proposal.challenge_period - 1));
    assert!(context.verify_tx(&too_early, 100_000_000).is_err());
    let mature = context.complete_tx(build(proposal.challenge_period));
    context.verify_tx(&mature, 100_000_000).unwrap();
}

#[test]
fn tally_state_encoding_used_by_test_is_canonical() {
    let state = TallyState {
        phase: TallyPhase::Active,
        proposal_id: [1; 32],
        operator_lock_hash: [2; 32],
        sequence: 0,
        next_block: 10,
        next_tx_index: 0,
        votes_root: [0; 32],
        dao_root: [0; 32],
        events_root: [0; 32],
        yes: 0,
        no: 0,
        processed_events: 0,
        candidate_since: 0,
    };
    assert_eq!(TallyState::decode(&state.encode()).unwrap(), state);
}

#[test]
fn treasury_payout_preserves_treasury_capacity() {
    let mut context = Context::default();
    let treasury_code = context.deploy_cell_by_name("treasury-lock-script");
    let always_success = context.deploy_cell(ALWAYS_SUCCESS.clone());
    let ordinary_lock = context
        .build_script(&always_success, Bytes::from(vec![1]))
        .unwrap();
    let receiver_lock = context
        .build_script(&always_success, Bytes::from(vec![2]))
        .unwrap();
    let zero_lock = context
        .build_script(&always_success, Bytes::from(vec![0]))
        .unwrap();
    let config_type = context
        .build_script(&always_success, Bytes::from(vec![3]))
        .unwrap();
    let result_type = context
        .build_script(&always_success, Bytes::from(vec![4]))
        .unwrap();
    let treasury_lock = context
        .build_script(&treasury_code, config_type.calc_script_hash().as_bytes())
        .unwrap();
    let treasury_config = TreasuryConfig {
        burn_expiry_blocks: 100,
        base_burn_incentive: 100 * CKB,
        burn_incentive_rate: 0,
        maximum_burn_incentive: 100 * CKB,
        result_type_hash: result_type
            .calc_script_hash()
            .as_slice()
            .try_into()
            .unwrap(),
        zero_lock_hash: zero_lock.calc_script_hash().as_slice().try_into().unwrap(),
    };
    let config_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(ordinary_lock.clone())
            .type_(Some(config_type).pack())
            .build(),
        Bytes::from(treasury_config.encode()),
    );
    let treasury_input = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(treasury_lock.clone())
            .build(),
        Bytes::new(),
    );
    let result = ResultData {
        passed: true,
        proposal_id: [1; 32],
        requested_amount: 600 * CKB,
        receiver_lock_hash: receiver_lock
            .calc_script_hash()
            .as_slice()
            .try_into()
            .unwrap(),
        yes: 10,
        no: 0,
        final_state_hash: [2; 32],
        proposal_config_data_hash: blake2b_256(
            &ProposalConfig {
                approval_bps: 6000,
                minimum_total_votes: 1,
                maximum_proposal_amount: 1_000 * CKB,
                minimum_challenge_period: 5,
                treasury_lock_hash: treasury_lock
                    .calc_script_hash()
                    .as_slice()
                    .try_into()
                    .unwrap(),
                dao_code_hash: [9; 32],
                dao_hash_type: 1,
                proposal_code_hash: [10; 32],
                proposal_hash_type: 1,
                vote_code_hash: [11; 32],
                vote_hash_type: 1,
                tally_code_hash: [12; 32],
                tally_hash_type: 1,
                candidate_lock_hash: [14; 32],
                policy_type_hash: [13; 32],
            }
            .encode(),
        ),
    };
    let result_input = context.create_cell(
        CellOutput::new_builder()
            .capacity(100 * CKB)
            .lock(ordinary_lock)
            .type_(Some(result_type).pack())
            .build(),
        Bytes::from(result.encode()),
    );
    let witness = WitnessArgs::new_builder()
        .lock(Some(Bytes::from(vec![1])).pack())
        .build();
    let tx = TransactionBuilder::default()
        .cell_dep(CellDep::new_builder().out_point(config_cell).build())
        .input(
            CellInput::new_builder()
                .previous_output(treasury_input)
                .build(),
        )
        .input(
            CellInput::new_builder()
                .previous_output(result_input)
                .build(),
        )
        .output(
            CellOutput::new_builder()
                .capacity(600 * CKB)
                .lock(receiver_lock)
                .build(),
        )
        .output(
            CellOutput::new_builder()
                .capacity(400 * CKB)
                .lock(treasury_lock)
                .build(),
        )
        .output_data(Bytes::new().pack())
        .output_data(Bytes::new().pack())
        .witness(witness.as_bytes().pack())
        .build();
    let tx = context.complete_tx(tx);
    context.verify_tx(&tx, 20_000_000).unwrap();
}

#[test]
fn expired_treasury_cell_can_be_burned_with_capped_incentive() {
    let mut context = Context::default();
    let treasury_code = context.deploy_cell_by_name("treasury-lock-script");
    let always_success = context.deploy_cell(ALWAYS_SUCCESS.clone());
    let zero_lock = context
        .build_script(&always_success, Bytes::from(vec![0]))
        .unwrap();
    let caller_lock = context
        .build_script(&always_success, Bytes::from(vec![1]))
        .unwrap();
    let config_type = context
        .build_script(&always_success, Bytes::from(vec![3]))
        .unwrap();
    let result_type = context
        .build_script(&always_success, Bytes::from(vec![4]))
        .unwrap();
    let treasury_lock = context
        .build_script(&treasury_code, config_type.calc_script_hash().as_bytes())
        .unwrap();
    let treasury_config = TreasuryConfig {
        burn_expiry_blocks: 10,
        base_burn_incentive: 100 * CKB,
        burn_incentive_rate: 10 * CKB,
        maximum_burn_incentive: 120 * CKB,
        result_type_hash: result_type
            .calc_script_hash()
            .as_slice()
            .try_into()
            .unwrap(),
        zero_lock_hash: zero_lock.calc_script_hash().as_slice().try_into().unwrap(),
    };
    let config_cell = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(caller_lock.clone())
            .type_(Some(config_type).pack())
            .build(),
        Bytes::from(treasury_config.encode()),
    );
    let treasury_input = context.create_cell(
        CellOutput::new_builder()
            .capacity(1_000 * CKB)
            .lock(treasury_lock)
            .build(),
        Bytes::new(),
    );
    let caller_hash = caller_lock.calc_script_hash();
    let mut action = vec![0];
    action.extend_from_slice(caller_hash.as_slice());
    let witness = WitnessArgs::new_builder()
        .lock(Some(Bytes::from(action)).pack())
        .build();
    let tx = TransactionBuilder::default()
        .cell_dep(CellDep::new_builder().out_point(config_cell).build())
        .input(
            CellInput::new_builder()
                .since(0x8000_0000_0000_000cu64)
                .previous_output(treasury_input)
                .build(),
        )
        .output(
            CellOutput::new_builder()
                .capacity(880 * CKB)
                .lock(zero_lock)
                .build(),
        )
        .output(
            CellOutput::new_builder()
                .capacity(120 * CKB)
                .lock(caller_lock)
                .build(),
        )
        .output_data(Bytes::new().pack())
        .output_data(Bytes::new().pack())
        .witness(witness.as_bytes().pack())
        .build();
    let tx = context.complete_tx(tx);
    context.verify_tx(&tx, 20_000_000).unwrap();
}
